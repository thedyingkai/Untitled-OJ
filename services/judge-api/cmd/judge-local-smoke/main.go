package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/redis/go-redis/v9"
	sharedjwt "ojos-shared/security/jwt"
)

var smokeHTTP = &http.Client{
	Transport: &http.Transport{Proxy: nil},
	Timeout:   60 * time.Second,
}

const (
	taskStream           = "ojos:judge:task"
	resultStream         = "ojos:judge:result"
	consumerGroup        = "judge-worker"
	childNodeID          = "child-node"
	rootNodeID           = "root-node"
	storageService       = "storage-service"
	workerService        = "judge-worker"
	judgeAPIService      = "judge-api"
	serviceToken         = "ojos-smoke-internal"
	workerToken          = "ojos-smoke-worker"
	workerEndpointID     = "127.0.0.2_19000_judge-worker"
	submissionSourceCode = "#include <iostream>\nint main() { std::cout << \"ok\\n\"; return 0; }\n"
)

type smokeConfig struct {
	repoRoot         string
	workRoot         string
	mode             string
	redisURL         string
	controlPlaneMode string
	authMode         string
	installMode      string
	storageBackend   string
	gatewayAdminJWT  string
	orchestrator     endpoint
	auth             endpoint
	storage          endpoint
	gateway          endpoint
	judgeAPI         endpoint
	timeout          time.Duration
	cleanStreams     bool
	lastTaskID       string
	lastResultID     string
	authStubCalls    *authCallRecorder
}

type endpoint struct {
	host string
	port int
}

func (e endpoint) baseURL() string {
	return fmt.Sprintf("http://%s:%d", e.host, e.port)
}

type stepError struct {
	step string
	err  error
}

func (e stepError) Error() string {
	return e.step + ": " + e.err.Error()
}

func (e stepError) Unwrap() error {
	return e.err
}

func main() {
	var (
		redisURL       = flag.String("redis", envDefault("OJOS_REAL_REDIS_URL", envDefault("REDIS_URL", "redis://127.0.0.1:6379/0")), "Redis URL for live smoke")
		controlPlane   = flag.String("control-plane", envDefault("OJOS_SMOKE_CONTROL_PLANE", "stub"), "control plane mode: stub or real")
		authMode       = flag.String("auth", envDefault("OJOS_SMOKE_AUTH", "stub"), "auth mode: stub or real")
		installMode    = flag.String("install-mode", envDefault("OJOS_SMOKE_INSTALL_MODE", ""), "install mode: seed or release-install")
		mode           = flag.String("mode", envDefault("OJOS_JUDGE_SMOKE_MODE", ""), "smoke matrix mode: beta-local")
		storageBackend = flag.String("storage-backend", envDefault("OJOS_SMOKE_STORAGE_BACKEND", "local"), "storage backend for storage-service: local or minio")
		workRoot       = flag.String("work-root", "", "smoke workspace; defaults to <repo>/.smoke/judge-local")
		timeout        = flag.Duration("timeout", 90*time.Second, "overall smoke timeout")
		cleanStreams   = flag.Bool("clean-streams", true, "delete judge task/result stream keys before the smoke")
	)
	flag.Parse()

	repoRoot, err := findRepoRoot()
	if err != nil {
		fmt.Fprintf(os.Stderr, "[FAIL] repo root\nreason: %v\n", err)
		os.Exit(1)
	}
	if strings.TrimSpace(*workRoot) == "" {
		*workRoot = filepath.Join(repoRoot, ".smoke", "judge-local")
	}
	normalizedMatrixMode := normalizeMatrixMode(*mode)
	if normalizedMatrixMode != "" && normalizedMatrixMode != "beta-local" {
		fmt.Fprintf(os.Stderr, "[FAIL] smoke mode\nreason: unsupported matrix mode %q\n", normalizedMatrixMode)
		os.Exit(1)
	}
	if normalizedMatrixMode == "beta-local" {
		*controlPlane = "real"
		*authMode = "real"
		*installMode = "release-install"
		if *timeout < 240*time.Second {
			*timeout = 240 * time.Second
		}
	}
	normalizedStorageBackend := normalizeStorageBackend(*storageBackend)
	if normalizedStorageBackend != "local" && normalizedStorageBackend != "minio" {
		fmt.Fprintf(os.Stderr, "[FAIL] storage backend\nreason: unsupported storage backend %q\n", normalizedStorageBackend)
		os.Exit(1)
	}
	orchestratorEndpoint, authEndpoint, storageEndpoint, gatewayEndpoint, judgeAPIEndpoint, err := allocateSmokeEndpoints()
	if err != nil {
		fmt.Fprintf(os.Stderr, "[FAIL] allocate smoke ports\nreason: %v\n", err)
		os.Exit(1)
	}
	cfg := smokeConfig{
		repoRoot:         repoRoot,
		workRoot:         *workRoot,
		mode:             normalizedMatrixMode,
		redisURL:         normalizeRedisURL(*redisURL),
		controlPlaneMode: normalizeSmokeMode(*controlPlane),
		authMode:         normalizeSmokeMode(*authMode),
		storageBackend:   normalizedStorageBackend,
		orchestrator:     orchestratorEndpoint,
		auth:             authEndpoint,
		storage:          storageEndpoint,
		gateway:          gatewayEndpoint,
		judgeAPI:         judgeAPIEndpoint,
		timeout:          *timeout,
		cleanStreams:     *cleanStreams,
		authStubCalls:    newAuthCallRecorder(),
	}
	smokeHTTP.Timeout = cfg.timeout
	if cfg.controlPlaneMode != cfg.authMode {
		fmt.Fprintf(os.Stderr, "[FAIL] smoke mode\nreason: mixed control-plane/auth modes are not supported; use both stub or both real\n")
		os.Exit(1)
	}
	if cfg.controlPlaneMode != "stub" && cfg.controlPlaneMode != "real" {
		fmt.Fprintf(os.Stderr, "[FAIL] smoke mode\nreason: unsupported control-plane mode %q\n", cfg.controlPlaneMode)
		os.Exit(1)
	}
	if cfg.authMode != "stub" && cfg.authMode != "real" {
		fmt.Fprintf(os.Stderr, "[FAIL] smoke mode\nreason: unsupported auth mode %q\n", cfg.authMode)
		os.Exit(1)
	}
	normalizedInstallMode := normalizeInstallMode(*installMode, cfg.controlPlaneMode)
	if normalizedInstallMode != "seed" && normalizedInstallMode != "release-install" {
		fmt.Fprintf(os.Stderr, "[FAIL] install mode\nreason: unsupported install mode %q\n", normalizedInstallMode)
		os.Exit(1)
	}
	cfg.installMode = normalizedInstallMode
	if cfg.controlPlaneMode == "stub" && cfg.installMode == "release-install" {
		fmt.Fprintf(os.Stderr, "[FAIL] install mode\nreason: release-install mode requires -control-plane real\n")
		os.Exit(1)
	}
	adminJWT, err := sharedjwt.Generate("smoke", 1, "smoke-admin", []string{"admin"}, 24)
	if err != nil {
		fmt.Fprintf(os.Stderr, "[FAIL] gateway admin token\nreason: %v\n", err)
		os.Exit(1)
	}
	cfg.gatewayAdminJWT = adminJWT

	ctx, cancel := context.WithTimeout(context.Background(), cfg.timeout)
	defer cancel()

	if err := run(ctx, cfg); err != nil {
		var step stepError
		if errors.As(err, &step) {
			fmt.Fprintf(os.Stderr, "[FAIL] %s\nreason: %v\n", step.step, step.err)
		} else {
			fmt.Fprintf(os.Stderr, "[FAIL] smoke\nreason: %v\n", err)
		}
		printLastLogs(os.Stderr, filepath.Join(cfg.workRoot, "logs"))
		os.Exit(1)
	}
	if cfg.mode == "beta-local" {
		printBetaMatrix(ctx, cfg)
	}
}

func run(ctx context.Context, cfg smokeConfig) error {
	if err := cleanupStaleSmokeProcesses(cfg.workRoot); err != nil {
		return fail("cleanup stale smoke processes", err)
	}
	if err := prepareWorkRoot(cfg.workRoot); err != nil {
		return fail("prepare smoke workspace", err)
	}
	storageCfg, err := writeStorageConfig(cfg)
	if err != nil {
		return fail("storage-service config", err)
	}

	redisClient, err := connectRedis(ctx, cfg.redisURL)
	if err != nil {
		return fail("redis connected", err)
	}
	defer redisClient.Close()
	if cfg.cleanStreams {
		if err := redisClient.Del(ctx, taskStream, resultStream).Err(); err != nil {
			return fail("redis streams cleaned", err)
		}
	}
	ok("redis connected")

	processes := make([]*childProcess, 0, 4)
	defer func() {
		for i := len(processes) - 1; i >= 0; i-- {
			processes[i].Stop()
		}
		_ = cleanupStaleSmokeProcesses(cfg.workRoot)
	}()

	var stub *http.Server
	if cfg.controlPlaneMode == "stub" || cfg.authMode == "stub" {
		var err error
		stub, err = startOrchestratorAuthStub(cfg)
		if err != nil {
			return fail("orchestrator/auth stub health", err)
		}
		defer shutdownHTTPServer(stub)
		if err := waitHealth(ctx, cfg.orchestrator.baseURL()+"/health"); err != nil {
			return fail("orchestrator/auth stub health", err)
		}
		ok("orchestrator/auth stub health")
	}

	if cfg.controlPlaneMode == "real" {
		orchestratorProc, err := startRealOrchestrator(ctx, cfg)
		if err != nil {
			return fail("orchestrator real backend started", err)
		}
		processes = append(processes, orchestratorProc)
		if err := waitProcessHealth(ctx, orchestratorProc, cfg.orchestrator.baseURL()+"/health"); err != nil {
			return fail("orchestrator real backend started", err)
		}
		ok("orchestrator real backend started")
	}

	if cfg.authMode == "real" {
		authProc, err := startRealAuthService(ctx, cfg)
		if err != nil {
			return fail("auth-service real server started", err)
		}
		processes = append(processes, authProc)
		if err := waitProcessHealth(ctx, authProc, cfg.auth.baseURL()+"/health"); err != nil {
			return fail("auth-service real server started", err)
		}
		ok("auth-service real server started")
		if cfg.installMode == "seed" {
			if err := verifyRealAuth(ctx, cfg); err != nil {
				return err
			}
		} else if err := verifyRealAuthMissingToken(ctx, cfg); err != nil {
			return err
		}
	}

	if cfg.installMode == "release-install" {
		ok("storage-service start deferred to release.install")
	} else {
		storageProc, err := startProcess(ctx, processSpec{
			name:    "storage-service",
			dir:     filepath.Join(cfg.repoRoot, "services", "storage-service"),
			logPath: filepath.Join(cfg.workRoot, "logs", "storage-service.log"),
			args:    []string{"go", "run", ".", "-f", storageCfg},
		})
		if err != nil {
			return fail("storage-service start", err)
		}
		processes = append(processes, storageProc)
		if err := waitProcessHealth(ctx, storageProc, cfg.storage.baseURL()+"/health"); err != nil {
			return fail("storage-service health", err)
		}
		ok("storage-service health")
	}

	if cfg.controlPlaneMode == "real" && cfg.installMode == "seed" {
		if err := seedRealOrchestrator(ctx, cfg); err != nil {
			return err
		}
	} else if cfg.controlPlaneMode == "real" && cfg.installMode == "release-install" {
		if err := seedRealNodeTree(ctx, cfg); err != nil {
			return err
		}
	}

	gatewayCfg, err := writeGatewayConfig(cfg)
	if err != nil {
		return fail("gateway config", err)
	}
	gatewayProc, err := startProcess(ctx, processSpec{
		name:    "gateway",
		dir:     filepath.Join(cfg.repoRoot, "services", "gateway"),
		logPath: filepath.Join(cfg.workRoot, "logs", "gateway.log"),
		args:    []string{"go", "run", ".", "-f", gatewayCfg},
		env:     noProxyEnv(nil),
	})
	if err != nil {
		return fail("gateway start", err)
	}
	processes = append(processes, gatewayProc)
	if err := waitProcessHealth(ctx, gatewayProc, cfg.gateway.baseURL()+"/health"); err != nil {
		return fail("gateway health", err)
	}
	ok("gateway health")

	if cfg.controlPlaneMode == "real" && cfg.installMode == "release-install" {
		if err := verifyStorageRouteAbsentBeforeInstall(ctx, cfg); err != nil {
			return err
		}
		storageEndpointID, err := installStorageRelease(ctx, cfg)
		if err != nil {
			return err
		}
		if err := installJudgeCallerIdentities(ctx, cfg); err != nil {
			return err
		}
		if err := waitGatewayRoute(ctx, cfg, "storage.object.put", storageEndpointID); err != nil {
			return fail("gateway route reload completed", err)
		}
		if err := verifyOrchestratorDrivenGatewayReload(ctx, cfg); err != nil {
			return err
		}
		ok("gateway route reload completed")
		if err := verifyStorageBackend(ctx, cfg); err != nil {
			return err
		}
		if err := verifyStorageLifecycleThroughResolver(ctx, cfg); err != nil {
			return err
		}
	}
	if cfg.authMode == "real" {
		if err := verifyRealAuth(ctx, cfg); err != nil {
			return err
		}
		if err := verifyStoragePermissionsRegistered(ctx, cfg); err != nil {
			return err
		}
		if err := verifyGatewayAuthBoundaries(ctx, cfg); err != nil {
			return err
		}
	}

	judgeProc, err := startProcess(ctx, processSpec{
		name:    "judge-api",
		dir:     filepath.Join(cfg.repoRoot, "services", "judge-api"),
		logPath: filepath.Join(cfg.workRoot, "logs", "judge-api.log"),
		args: []string{
			"go", "run", "./cmd/smoke-server",
			"-port", strconv.Itoa(cfg.judgeAPI.port),
			"-redis", cfg.redisURL,
			"-internal-gateway", cfg.gateway.baseURL(),
			"-worker-token", workerToken,
			"-service-token", serviceToken,
			"-caller-node-id", childNodeID,
			"-submissions-root", filepath.Join(cfg.workRoot, "submissions"),
		},
		env: noProxyEnv(nil),
	})
	if err != nil {
		return fail("judge-api start", err)
	}
	processes = append(processes, judgeProc)
	if err := waitProcessHealth(ctx, judgeProc, cfg.judgeAPI.baseURL()+"/health"); err != nil {
		return fail("judge-api health", err)
	}
	ok("judge-api health")

	submissionID, err := createSubmission(ctx, cfg)
	if err != nil {
		return fail("submission created", err)
	}
	ok("submission created: %d", submissionID)

	sourceKey := fmt.Sprintf("%d-source-main.cpp", submissionID)
	if err := waitStorageObject(ctx, cfg, "submissions", sourceKey, submissionSourceCode); err != nil {
		return fail("source stored through internal resolver", err)
	}
	if err := waitLogContains(ctx, filepath.Join(cfg.workRoot, "logs", "gateway.log"), fmt.Sprintf("/internal/apis/storage.object.put/submissions/%d-source-main.cpp", submissionID)); err != nil {
		return fail("source stored through internal resolver", err)
	}
	ok("source stored through internal resolver")

	taskID, err := findTaskEntry(ctx, redisClient, submissionID)
	if err != nil {
		return fail("redis task written", err)
	}
	cfg.lastTaskID = taskID
	ok("redis task written: %s", taskID)

	workerProc, err := startWorker(ctx, cfg)
	if err != nil {
		return fail("worker start", err)
	}
	processes = append(processes, workerProc)
	if err := workerProc.Wait(60 * time.Second); err != nil {
		return fail("worker consumed task", err)
	}
	if err := waitLogContains(ctx, filepath.Join(cfg.workRoot, "logs", "judge-worker.log"), "claimed worker task"); err != nil {
		return fail("worker consumed task", err)
	}
	ok("worker consumed task")

	pending, err := pendingCount(ctx, redisClient)
	if err != nil {
		return fail("worker consumed task acked", err)
	}
	if pending != 0 {
		return fail("worker consumed task acked", fmt.Errorf("pending count is %d", pending))
	}
	ok("worker consumed task acked")
	if err := verifyQueueStatusAPI(ctx, cfg); err != nil {
		return err
	}

	resultID, resultStatus, err := findResultEntry(ctx, redisClient, submissionID)
	if err != nil {
		return fail("result written", err)
	}
	cfg.lastResultID = resultID
	if resultStatus != "ACCEPTED" {
		return fail("result written", fmt.Errorf("unexpected result status %q", resultStatus))
	}
	resultKey := fmt.Sprintf("%d-result.json", submissionID)
	if err := waitStorageObject(ctx, cfg, "submissions", resultKey, "ACCEPTED"); err != nil {
		return fail("result written", err)
	}
	ok("result written")

	status, err := waitSubmissionStatus(ctx, cfg, submissionID)
	if err != nil {
		return fail("result query returned Finished/Accepted", err)
	}
	if status != "ACCEPTED" && status != "FINISHED" && status != "Finished" {
		return fail("result query returned Finished/Accepted", fmt.Errorf("unexpected status %q", status))
	}
	if err := querySubmissionCases(ctx, cfg, submissionID); err != nil {
		return fail("result query returned Finished/Accepted", err)
	}
	ok("result query returned Finished/Accepted")

	if cfg.controlPlaneMode == "real" && cfg.installMode == "release-install" {
		if err := rollbackStorageReleaseInstall(ctx, cfg); err != nil {
			return err
		}
	}

	if cfg.authMode == "stub" && (!cfg.authStubCalls.HasCaller(judgeAPIService) || !cfg.authStubCalls.HasCaller(workerService)) {
		return fail("service identity observed", fmt.Errorf("auth stub calls: %v", cfg.authStubCalls.Snapshot()))
	}
	if cfg.authMode == "real" {
		if err := waitLogContains(ctx, filepath.Join(cfg.workRoot, "logs", "auth-service.log"), "POST /auth/permission-check"); err != nil {
			return fail("service identity observed", err)
		}
	}
	ok("service identity observed: judge-api and judge-worker")

	fmt.Printf("[OK] smoke summary: submission_id=%d task_entry_id=%s result_entry_id=%s status=%s\n", submissionID, taskID, resultID, status)
	fmt.Printf("[OK] smoke logs: %s\n", filepath.Join(cfg.workRoot, "logs"))
	return nil
}

func startWorker(ctx context.Context, cfg smokeConfig) (*childProcess, error) {
	env := noProxyEnv(map[string]string{
		"LANGUAGES_CONFIG":               filepath.Join(cfg.repoRoot, "services", "judge-worker", "config", "languages.yaml"),
		"OJOS_JUDGE_API_URL":             cfg.judgeAPI.baseURL(),
		"OJOS_WORKER_TOKEN":              workerToken,
		"OJOS_WORKER_ID":                 workerEndpointID,
		"OJOS_WORKER_NAME":               "smoke-judge-worker",
		"OJOS_MAX_CONCURRENCY":           "1",
		"OJOS_WORK_DIR":                  filepath.Join(cfg.workRoot, "worker-work"),
		"OJOS_ARTIFACT_CACHE_DIR":        filepath.Join(cfg.workRoot, "worker-cache"),
		"OJOS_SUPPORTED_LANGUAGES":       "cpp17",
		"OJOS_REDIS_URL":                 cfg.redisURL,
		"OJOS_JUDGE_TASK_STREAM":         taskStream,
		"OJOS_JUDGE_CONSUMER_GROUP":      consumerGroup,
		"OJOS_INTERNAL_GATEWAY_URL":      cfg.gateway.baseURL(),
		"OJOS_STORAGE_OBJECT_GET_API_ID": "storage.object.get",
		"OJOS_STORAGE_OBJECT_PUT_API_ID": "storage.object.put",
		"OJOS_SERVICE_TOKEN":             serviceToken,
		"OJOS_CALLER_NODE_ID":            childNodeID,
		"OJOS_RUNNER_MODE":               "fake",
		"OJOS_WORKER_SMOKE_ONCE":         "1",
	})
	return startProcess(ctx, processSpec{
		name:    "judge-worker",
		dir:     filepath.Join(cfg.repoRoot, "services", "judge-worker"),
		logPath: filepath.Join(cfg.workRoot, "logs", "judge-worker.log"),
		args:    []string{"cargo", "run", "--quiet"},
		env:     env,
	})
}

func startRealOrchestrator(ctx context.Context, cfg smokeConfig) (*childProcess, error) {
	return startProcess(ctx, processSpec{
		name:    "orchestrator",
		dir:     filepath.Join(cfg.repoRoot, "services", "orchestrator", "backend"),
		logPath: filepath.Join(cfg.workRoot, "logs", "orchestrator.log"),
		args: []string{
			"cargo", "run", "--quiet", "--",
			"--repo-root", cfg.repoRoot,
			"--bind", fmt.Sprintf("%s:%d", cfg.orchestrator.host, cfg.orchestrator.port),
		},
		env: noProxyEnv(map[string]string{
			"OJOS_SMOKE_MODE":                          "1",
			"ORCHESTRATOR_RELEASE_PACKAGE_LOAD":        "1",
			"ORCHESTRATOR_RELEASE_PACKAGE_ROOT":        cfg.repoRoot,
			"ORCHESTRATOR_AUTH_PERMISSION_SYNC":        "1",
			"AUTH_SERVICE_ENDPOINT":                    cfg.auth.baseURL(),
			"AUTH_SERVICE_ADMIN_TOKEN":                 serviceToken,
			"ORCHESTRATOR_GATEWAY_ROUTE_PUBLISH":       "1",
			"GATEWAY_ENDPOINT":                         cfg.gateway.baseURL(),
			"GATEWAY_ADMIN_TOKEN":                      cfg.gatewayAdminJWT,
			"GATEWAY_NODE_ID":                          childNodeID,
			"OJOS_STORAGE_SERVICE_CONFIG":              storageConfigPath(cfg),
			"OJOS_STORAGE_ROOT":                        filepath.Join(cfg.workRoot, "storage"),
			"OJOS_STORAGE_BUCKETS":                     "submissions,problems,judge-artifacts",
			"OJOS_LOCAL_PROCESS_STATE_DIR":             filepath.Join(cfg.workRoot, "local-process"),
			"ORCHESTRATOR_ENDPOINT_HEALTH_ATTEMPTS":    "120",
			"ORCHESTRATOR_ENDPOINT_HEALTH_INTERVAL_MS": "500",
		}),
	})
}

func startRealAuthService(ctx context.Context, cfg smokeConfig) (*childProcess, error) {
	authCfg, err := writeAuthConfig(cfg)
	if err != nil {
		return nil, err
	}
	return startProcess(ctx, processSpec{
		name:    "auth-service",
		dir:     filepath.Join(cfg.repoRoot, "services", "auth-service"),
		logPath: filepath.Join(cfg.workRoot, "logs", "auth-service.log"),
		args:    []string{"go", "run", ".", "-f", authCfg},
		env: noProxyEnv(map[string]string{
			"OJOS_SMOKE_MODE":     "1",
			"AUTH_INTERNAL_TOKEN": serviceToken,
		}),
	})
}

func seedRealOrchestrator(ctx context.Context, cfg smokeConfig) error {
	endpointID := fmt.Sprintf("%s:%d:%s", cfg.storage.host, cfg.storage.port, storageService)
	body := map[string]any{
		"root_node_id":         rootNodeID,
		"root_host_ip":         cfg.storage.host,
		"child_node_id":        childNodeID,
		"child_host_ip":        "127.0.0.2",
		"storage_service_name": storageService,
		"storage_version":      "0.1.0",
		"storage_endpoint":     endpointID,
		"storage_protocol":     "http",
	}
	var resp struct {
		Status        string               `json:"status"`
		NodeID        string               `json:"node_id"`
		EffectiveAPIs []effectiveAPIRecord `json:"effective_apis"`
	}
	if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.orchestrator.baseURL()+"/internal/smoke/seed-control-plane", body, map[string]string{}, &resp); err != nil {
		return fail("node tree seeded", err)
	}
	if resp.NodeID != childNodeID {
		return fail("node tree seeded", fmt.Errorf("unexpected seeded node %q", resp.NodeID))
	}
	ok("node tree seeded")
	for _, want := range []string{"storage.object.put", "storage.object.get", "storage.object.head", "storage.object.delete"} {
		if !effectiveAPIContains(resp.EffectiveAPIs, want, endpointID) {
			return fail("storage API surface registered", fmt.Errorf("missing effective API %s endpoint=%s", want, endpointID))
		}
	}
	ok("storage API surface registered")
	if err := verifyRealOrchestratorRoutes(ctx, cfg, endpointID); err != nil {
		return err
	}
	return nil
}

func seedRealNodeTree(ctx context.Context, cfg smokeConfig) error {
	nodes := []map[string]any{
		{
			"node_id":        rootNodeID,
			"host_ip":        cfg.storage.host,
			"parent_node_id": "",
			"role":           "root",
			"labels": map[string]any{
				"smoke": true,
			},
			"status": "running",
		},
		{
			"node_id":        childNodeID,
			"host_ip":        "127.0.0.2",
			"parent_node_id": rootNodeID,
			"role":           "node",
			"labels": map[string]any{
				"smoke": true,
			},
			"status": "running",
		},
	}
	for _, body := range nodes {
		var resp struct {
			Node struct {
				NodeID string `json:"node_id"`
				HostIP string `json:"host_ip"`
				Role   string `json:"role"`
			} `json:"node"`
		}
		if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.orchestrator.baseURL()+"/nodes", body, map[string]string{}, &resp); err != nil {
			return fail("nodes created through API", err)
		}
		if resp.Node.NodeID != body["node_id"] {
			return fail("nodes created through API", fmt.Errorf("unexpected node API response: got %q want %q", resp.Node.NodeID, body["node_id"]))
		}
	}
	var tree struct {
		Nodes []struct {
			NodeID string `json:"node_id"`
			HostIP string `json:"host_ip"`
			Role   string `json:"role"`
		} `json:"nodes"`
	}
	if err := doJSONWithHeaders(ctx, http.MethodGet, cfg.orchestrator.baseURL()+"/nodes", nil, map[string]string{}, &tree); err != nil {
		return fail("nodes created through API", err)
	}
	if len(tree.Nodes) < 2 {
		return fail("nodes created through API", fmt.Errorf("expected at least root and child nodes, got %d", len(tree.Nodes)))
	}
	ok("nodes created through API")
	return nil
}

func verifyStorageRouteAbsentBeforeInstall(ctx context.Context, cfg smokeConfig) error {
	if err := verifyRouteMissing(ctx, cfg, "storage.object.get"); err != nil {
		return fail("release.install before route absent", err)
	}
	status, body, err := doStatus(ctx, http.MethodGet, cfg.gateway.baseURL()+"/internal/apis/storage.object.get/submissions/pre-install.txt", nil, map[string]string{
		"Authorization":         "Bearer " + serviceToken,
		"X-OJOS-Caller-Service": judgeAPIService,
		"X-OJOS-Node-Id":        childNodeID,
	})
	if err != nil {
		return fail("release.install before route absent", err)
	}
	if status != http.StatusServiceUnavailable && status != http.StatusNotFound {
		return fail("release.install before route absent", fmt.Errorf("expected missing storage route before install, got %d: %s", status, strings.TrimSpace(string(body))))
	}
	ok("release.install before route absent")
	return nil
}

func installStorageRelease(ctx context.Context, cfg smokeConfig) (string, error) {
	endpointID := fmt.Sprintf("%s:%d:%s", cfg.storage.host, cfg.storage.port, storageService)
	body := map[string]any{
		"operation_id":             "op-smoke-storage-release-install",
		"host_ip":                  cfg.storage.host,
		"endpoint":                 endpointID,
		"gateway_node_id":          childNodeID,
		"execute_service_driver":   true,
		"external_service_running": false,
	}
	var installResp struct {
		ActionResult struct {
			ActionID    string `json:"action_id"`
			Status      string `json:"status"`
			Message     string `json:"message"`
			OperationID string `json:"operation_id"`
			Error       string `json:"error"`
		} `json:"action_result"`
	}
	if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.orchestrator.baseURL()+"/releases/"+storageService+"/install", body, map[string]string{}, &installResp); err != nil {
		return "", fail("release.install storage-service", err)
	}
	if installResp.ActionResult.ActionID != "release.install" {
		return "", fail("release.install storage-service", fmt.Errorf("unexpected action_id %q", installResp.ActionResult.ActionID))
	}
	if !strings.EqualFold(strings.ReplaceAll(installResp.ActionResult.Status, "_", ""), "succeeded") {
		return "", fail("release.install storage-service", fmt.Errorf("unexpected status %q operation_id=%s error=%s message=%s", installResp.ActionResult.Status, installResp.ActionResult.OperationID, installResp.ActionResult.Error, installResp.ActionResult.Message))
	}
	ok("release.install storage-service")

	if err := verifyRealOrchestratorRoutes(ctx, cfg, endpointID); err != nil {
		return "", err
	}
	return endpointID, nil
}

func installJudgeCallerIdentities(ctx context.Context, cfg smokeConfig) error {
	installs := []struct {
		serviceName string
		hostIP      string
		port        int
	}{
		{serviceName: judgeAPIService, hostIP: cfg.judgeAPI.host, port: cfg.judgeAPI.port},
		{serviceName: workerService, hostIP: "127.0.0.2", port: 9101},
	}
	for _, item := range installs {
		endpointID := fmt.Sprintf("%s:%d:%s", item.hostIP, item.port, item.serviceName)
		body := map[string]any{
			"operation_id":             "op-smoke-" + item.serviceName + "-identity-install",
			"host_ip":                  item.hostIP,
			"endpoint":                 endpointID,
			"gateway_node_id":          childNodeID,
			"execute_service_driver":   false,
			"external_service_running": true,
		}
		var installResp struct {
			ActionResult struct {
				ActionID    string `json:"action_id"`
				Status      string `json:"status"`
				Message     string `json:"message"`
				OperationID string `json:"operation_id"`
				Error       string `json:"error"`
			} `json:"action_result"`
		}
		if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.orchestrator.baseURL()+"/releases/"+item.serviceName+"/install", body, map[string]string{}, &installResp); err != nil {
			return fail("release.install service identities", err)
		}
		if installResp.ActionResult.ActionID != "release.install" {
			return fail("release.install service identities", fmt.Errorf("%s unexpected action_id %q", item.serviceName, installResp.ActionResult.ActionID))
		}
		if !strings.EqualFold(strings.ReplaceAll(installResp.ActionResult.Status, "_", ""), "succeeded") {
			return fail("release.install service identities", fmt.Errorf("%s unexpected status %q operation_id=%s error=%s message=%s", item.serviceName, installResp.ActionResult.Status, installResp.ActionResult.OperationID, installResp.ActionResult.Error, installResp.ActionResult.Message))
		}
	}
	ok("release.install service identities: judge-api and judge-worker")
	return nil
}

func verifyRealOrchestratorRoutes(ctx context.Context, cfg smokeConfig, endpointID string) error {
	var table routeTableResponse
	target := cfg.orchestrator.baseURL() + "/internal/orchestrator/nodes/" + childNodeID + "/routes?include_upstream=true"
	if err := doJSONWithHeaders(ctx, http.MethodGet, target, nil, map[string]string{}, &table); err != nil {
		return fail("child effective routes loaded from real orchestrator", err)
	}
	for _, want := range []string{"storage.object.put", "storage.object.get", "storage.object.head", "storage.object.delete"} {
		route := findRoute(table.Routes, want)
		if route == nil {
			return fail("child effective routes loaded from real orchestrator", fmt.Errorf("missing route %s", want))
		}
		if route.ProviderNodeID != rootNodeID || route.ProviderEndpoint != endpointID || route.UpstreamBase != cfg.storage.baseURL() {
			return fail("child effective routes loaded from real orchestrator", fmt.Errorf("route %s resolved to provider=%s endpoint=%s upstream=%s", want, route.ProviderNodeID, route.ProviderEndpoint, route.UpstreamBase))
		}
	}
	ok("child effective routes loaded from real orchestrator")
	return nil
}

func verifyRouteMissing(ctx context.Context, cfg smokeConfig, apiID string) error {
	var table routeTableResponse
	target := cfg.orchestrator.baseURL() + "/internal/orchestrator/nodes/" + childNodeID + "/routes?include_upstream=true"
	if err := doJSONWithHeaders(ctx, http.MethodGet, target, nil, map[string]string{}, &table); err != nil {
		return err
	}
	if findRoute(table.Routes, apiID) != nil {
		return fmt.Errorf("route %s existed before release.install", apiID)
	}
	return nil
}

func rollbackStorageReleaseInstall(ctx context.Context, cfg smokeConfig) error {
	var resp struct {
		ActionResult struct {
			ActionID    string `json:"action_id"`
			Status      string `json:"status"`
			OperationID string `json:"operation_id"`
			Error       string `json:"error"`
			Message     string `json:"message"`
		} `json:"action_result"`
	}
	target := cfg.orchestrator.baseURL() + "/operations/op-smoke-storage-release-install/rollback"
	if err := doJSONWithHeaders(ctx, http.MethodPost, target, map[string]any{}, map[string]string{}, &resp); err != nil {
		return fail("release.install rollback stopped storage-service", err)
	}
	normalizedStatus := strings.ToLower(strings.ReplaceAll(resp.ActionResult.Status, "_", ""))
	if normalizedStatus != "succeeded" && normalizedStatus != "rolledback" {
		return fail("release.install rollback stopped storage-service", fmt.Errorf("unexpected rollback status %q operation_id=%s error=%s message=%s", resp.ActionResult.Status, resp.ActionResult.OperationID, resp.ActionResult.Error, resp.ActionResult.Message))
	}
	if err := waitStorageUnavailable(ctx, cfg.storage.baseURL()+"/health"); err != nil {
		return fail("release.install rollback stopped storage-service", err)
	}
	ok("release.install rollback stopped storage-service")
	return nil
}

func waitGatewayRoute(ctx context.Context, cfg smokeConfig, apiID string, endpointID string) error {
	deadline := time.Now().Add(20 * time.Second)
	var last error
	for time.Now().Before(deadline) {
		status, body, err := doStatus(ctx, http.MethodPut, cfg.gateway.baseURL()+"/internal/apis/"+apiID+"/submissions/reload-probe.txt", map[string]string{
			"probe": "release-install-gateway-reload",
		}, map[string]string{
			"Authorization":         "Bearer " + serviceToken,
			"X-OJOS-Caller-Service": judgeAPIService,
			"X-OJOS-Node-Id":        childNodeID,
		})
		if err != nil {
			last = err
		} else if status >= 200 && status < 300 {
			return nil
		} else {
			last = fmt.Errorf("gateway route still unavailable for %s endpoint=%s status=%d body=%s", apiID, endpointID, status, strings.TrimSpace(string(body)))
		}
		if wait(ctx, 300*time.Millisecond) != nil {
			return ctx.Err()
		}
	}
	if last == nil {
		last = fmt.Errorf("gateway route %s did not appear", apiID)
	}
	return last
}

func verifyOrchestratorDrivenGatewayReload(ctx context.Context, cfg smokeConfig) error {
	var resp struct {
		Logs []struct {
			StepID  string `json:"step_id"`
			Level   string `json:"level"`
			Message string `json:"message"`
			Data    struct {
				Reloaded bool   `json:"reloaded"`
				Status   string `json:"status"`
			} `json:"data"`
		} `json:"logs"`
	}
	target := cfg.orchestrator.baseURL() + "/operations/op-smoke-storage-release-install/logs"
	if err := doJSONWithHeaders(ctx, http.MethodGet, target, nil, map[string]string{}, &resp); err != nil {
		return fail("gateway reload triggered by orchestrator", err)
	}
	for _, log := range resp.Logs {
		if log.StepID == "gateway_reload:storage-service" &&
			log.Data.Reloaded &&
			strings.EqualFold(log.Data.Status, "published") &&
			strings.Contains(log.Message, "gateway reload triggered by orchestrator") {
			ok("gateway reload triggered by orchestrator")
			return nil
		}
	}
	return fail("gateway reload triggered by orchestrator", fmt.Errorf("gateway reload operation log not found: %#v", resp.Logs))
}

func verifyStorageBackend(ctx context.Context, cfg smokeConfig) error {
	var health struct {
		Status  string `json:"status"`
		Backend string `json:"backend"`
	}
	if err := doJSONWithHeaders(ctx, http.MethodGet, cfg.storage.baseURL()+"/health", nil, map[string]string{}, &health); err != nil {
		return fail("storage backend "+cfg.storageBackend, err)
	}
	if !strings.EqualFold(health.Backend, cfg.storageBackend) {
		return fail("storage backend "+cfg.storageBackend, fmt.Errorf("expected backend %q, got status=%q backend=%q", cfg.storageBackend, health.Status, health.Backend))
	}
	ok("storage backend %s", cfg.storageBackend)
	return nil
}

func verifyStorageLifecycleThroughResolver(ctx context.Context, cfg smokeConfig) error {
	key := fmt.Sprintf("beta-%s-%d.txt", cfg.storageBackend, time.Now().UnixNano())
	body := "beta storage lifecycle via internal resolver\n"
	headers := map[string]string{
		"Authorization":         "Bearer " + serviceToken,
		"X-OJOS-Caller-Service": judgeAPIService,
		"X-OJOS-Node-Id":        childNodeID,
	}
	objectPath := "/internal/apis/storage.object.put/judge-artifacts/" + key
	status, respBody, err := doStatus(ctx, http.MethodPut, cfg.gateway.baseURL()+objectPath, body, headers)
	if err != nil {
		return fail("storage lifecycle via internal resolver", err)
	}
	if status < 200 || status >= 300 {
		return fail("storage lifecycle via internal resolver", fmt.Errorf("PUT got %d: %s", status, strings.TrimSpace(string(respBody))))
	}

	status, respBody, err = doStatus(ctx, http.MethodHead, cfg.gateway.baseURL()+"/internal/apis/storage.object.head/judge-artifacts/"+key, nil, headers)
	if err != nil {
		return fail("storage lifecycle via internal resolver", err)
	}
	if status < 200 || status >= 300 {
		return fail("storage lifecycle via internal resolver", fmt.Errorf("HEAD got %d: %s", status, strings.TrimSpace(string(respBody))))
	}

	status, respBody, err = doStatus(ctx, http.MethodGet, cfg.gateway.baseURL()+"/internal/apis/storage.object.get/judge-artifacts/"+key, nil, headers)
	if err != nil {
		return fail("storage lifecycle via internal resolver", err)
	}
	if status < 200 || status >= 300 || string(respBody) != body {
		return fail("storage lifecycle via internal resolver", fmt.Errorf("GET got status=%d body=%q", status, string(respBody)))
	}

	status, respBody, err = doStatus(ctx, http.MethodDelete, cfg.storage.baseURL()+"/api/storage/objects/judge-artifacts/"+key, nil, map[string]string{})
	if err != nil {
		return fail("storage lifecycle delete through storage HTTP", err)
	}
	if status < 200 || status >= 300 {
		return fail("storage lifecycle delete through storage HTTP", fmt.Errorf("DELETE got %d: %s", status, strings.TrimSpace(string(respBody))))
	}
	status, respBody, err = doStatus(ctx, http.MethodGet, cfg.gateway.baseURL()+"/internal/apis/storage.object.get/judge-artifacts/"+key, nil, headers)
	if err != nil {
		return fail("storage lifecycle delete through storage HTTP", err)
	}
	if status != http.StatusNotFound && !(status == http.StatusBadRequest && strings.Contains(strings.ToLower(string(respBody)), "key does not exist")) {
		return fail("storage lifecycle delete through storage HTTP", fmt.Errorf("expected GET after delete to return 404, got %d: %s", status, strings.TrimSpace(string(respBody))))
	}
	ok("storage lifecycle via internal resolver and storage HTTP delete")
	return nil
}

func waitStorageObject(ctx context.Context, cfg smokeConfig, bucket string, key string, wantSubstring string) error {
	headers := map[string]string{
		"Authorization":         "Bearer " + serviceToken,
		"X-OJOS-Caller-Service": judgeAPIService,
		"X-OJOS-Node-Id":        childNodeID,
	}
	target := cfg.gateway.baseURL() + "/internal/apis/storage.object.get/" + bucket + "/" + key
	deadline := time.Now().Add(20 * time.Second)
	var last error
	for time.Now().Before(deadline) {
		status, body, err := doStatus(ctx, http.MethodGet, target, nil, headers)
		if err != nil {
			last = err
		} else if status >= 200 && status < 300 {
			if wantSubstring == "" || strings.Contains(string(body), wantSubstring) {
				return nil
			}
			last = fmt.Errorf("object %s/%s did not contain expected text", bucket, key)
		} else {
			last = fmt.Errorf("object %s/%s unavailable: status=%d body=%s", bucket, key, status, strings.TrimSpace(string(body)))
		}
		if wait(ctx, 300*time.Millisecond) != nil {
			return ctx.Err()
		}
	}
	if last == nil {
		last = fmt.Errorf("object %s/%s did not appear", bucket, key)
	}
	return last
}

func verifyRealAuth(ctx context.Context, cfg smokeConfig) error {
	allowed, status, err := permissionCheck(ctx, cfg.auth.baseURL(), serviceToken, judgeAPIService, "storage.object.put", "storage.object.write")
	if err != nil {
		return fail("permission-check allowed service caller", err)
	}
	if status != http.StatusOK || !allowed {
		return fail("permission-check allowed service caller", fmt.Errorf("status=%d allowed=%v", status, allowed))
	}
	ok("permission-check allowed service caller")

	_, status, err = permissionCheck(ctx, cfg.auth.baseURL(), "", judgeAPIService, "storage.object.put", "storage.object.write")
	if err != nil {
		return fail("permission-check rejected missing token", err)
	}
	if status != http.StatusUnauthorized {
		return fail("permission-check rejected missing token", fmt.Errorf("expected 401, got %d", status))
	}
	ok("permission-check rejected missing token")

	allowed, status, err = permissionCheck(ctx, cfg.auth.baseURL(), serviceToken, judgeAPIService, "storage.object.delete", "storage.object.delete")
	if err != nil {
		return fail("permission-check denied missing permission", err)
	}
	if status != http.StatusOK || allowed {
		return fail("permission-check denied missing permission", fmt.Errorf("status=%d allowed=%v", status, allowed))
	}
	ok("permission-check denied missing permission")

	allowed, status, err = permissionCheck(ctx, cfg.auth.baseURL(), serviceToken, "fake-worker", "storage.object.get", "storage.object.read")
	if err != nil {
		return fail("permission-check denied unknown service", err)
	}
	if status != http.StatusOK || allowed {
		return fail("permission-check denied unknown service", fmt.Errorf("status=%d allowed=%v", status, allowed))
	}
	ok("permission-check denied unknown service")
	return nil
}

func verifyRealAuthMissingToken(ctx context.Context, cfg smokeConfig) error {
	_, status, err := permissionCheck(ctx, cfg.auth.baseURL(), "", judgeAPIService, "storage.object.put", "storage.object.write")
	if err != nil {
		return fail("permission-check rejected missing token", err)
	}
	if status != http.StatusUnauthorized {
		return fail("permission-check rejected missing token", fmt.Errorf("expected 401, got %d", status))
	}
	ok("permission-check rejected missing token")
	return nil
}

func verifyStoragePermissionsRegistered(ctx context.Context, cfg smokeConfig) error {
	var resp struct {
		Code int `json:"code"`
		Data []struct {
			Code        string `json:"code"`
			ServiceCode string `json:"service_code"`
		} `json:"data"`
	}
	headers := map[string]string{"Authorization": "Bearer " + serviceToken}
	if err := doJSONWithHeaders(ctx, http.MethodGet, cfg.auth.baseURL()+"/auth/admin/permissions", nil, headers, &resp); err != nil {
		return fail("storage permissions registered into auth-service", err)
	}
	want := map[string]bool{
		"storage.object.read":   false,
		"storage.object.write":  false,
		"storage.object.delete": false,
	}
	for _, item := range resp.Data {
		if item.ServiceCode != storageService {
			continue
		}
		if _, ok := want[item.Code]; ok {
			want[item.Code] = true
		}
	}
	for code, found := range want {
		if !found {
			return fail("storage permissions registered into auth-service", fmt.Errorf("missing permission %s", code))
		}
	}
	ok("storage permissions registered into auth-service")
	return nil
}

func verifyGatewayAuthBoundaries(ctx context.Context, cfg smokeConfig) error {
	headers := map[string]string{
		"Authorization":         "Bearer " + serviceToken,
		"X-OJOS-Caller-Service": judgeAPIService,
		"X-OJOS-Node-Id":        childNodeID,
	}
	status, body, err := doStatus(ctx, http.MethodDelete, cfg.gateway.baseURL()+"/internal/apis/storage.object.delete/submissions/auth-denied.txt", nil, headers)
	if err != nil {
		return fail("gateway permission-check denied missing permission", err)
	}
	if status != http.StatusForbidden {
		return fail("gateway permission-check denied missing permission", fmt.Errorf("expected 403, got %d: %s", status, strings.TrimSpace(string(body))))
	}
	ok("gateway permission-check denied missing permission")

	headers["X-OJOS-Caller-Service"] = "fake-worker"
	status, body, err = doStatus(ctx, http.MethodGet, cfg.gateway.baseURL()+"/internal/apis/storage.object.get/submissions/auth-unknown.txt", nil, headers)
	if err != nil {
		return fail("gateway permission-check denied unknown service", err)
	}
	if status != http.StatusForbidden {
		return fail("gateway permission-check denied unknown service", fmt.Errorf("expected 403, got %d: %s", status, strings.TrimSpace(string(body))))
	}
	ok("gateway permission-check denied unknown service")
	headers["X-OJOS-Caller-Service"] = judgeAPIService

	status, body, err = doStatus(ctx, http.MethodGet, cfg.gateway.baseURL()+"/internal/apis/storage.object.get/submissions/auth-missing.txt", nil, map[string]string{
		"X-OJOS-Caller-Service": judgeAPIService,
		"X-OJOS-Node-Id":        childNodeID,
	})
	if err != nil {
		return fail("gateway permission-check rejected missing token", err)
	}
	if status != http.StatusUnauthorized {
		return fail("gateway permission-check rejected missing token", fmt.Errorf("expected 401, got %d: %s", status, strings.TrimSpace(string(body))))
	}
	ok("gateway permission-check rejected missing token")

	if cfg.installMode == "release-install" {
		return nil
	}
	status, body, err = doStatus(ctx, http.MethodGet, cfg.gateway.baseURL()+"/internal/apis/storage.object.public-head/submissions/auth-public-missing.txt", nil, map[string]string{
		"X-OJOS-Node-Id": childNodeID,
	})
	if err != nil {
		return fail("public API skipped token", err)
	}
	if status == http.StatusUnauthorized || status == http.StatusForbidden {
		return fail("public API skipped token", fmt.Errorf("expected non-auth response, got %d: %s", status, strings.TrimSpace(string(body))))
	}
	ok("public API skipped token")
	return nil
}

func permissionCheck(ctx context.Context, baseURL string, token string, callerService string, apiID string, permission string) (bool, int, error) {
	body := map[string]any{
		"caller_type":    "service",
		"caller_service": callerService,
		"caller_node_id": childNodeID,
		"api_id":         apiID,
		"permission":     permission,
		"scope_type":     "system",
	}
	headers := map[string]string{}
	if strings.TrimSpace(token) != "" {
		headers["Authorization"] = "Bearer " + token
	}
	status, data, err := doStatus(ctx, http.MethodPost, strings.TrimRight(baseURL, "/")+"/auth/permission-check", body, headers)
	if err != nil {
		return false, 0, err
	}
	if status < 200 || status >= 300 {
		return false, status, nil
	}
	var resp struct {
		Code int `json:"code"`
		Data struct {
			Allowed bool `json:"allowed"`
		} `json:"data"`
	}
	if err := json.Unmarshal(data, &resp); err != nil {
		return false, status, err
	}
	return resp.Code == 0 && resp.Data.Allowed, status, nil
}

type effectiveAPIRecord struct {
	APIID            string `json:"api_id"`
	ProviderEndpoint string `json:"provider_endpoint"`
	Status           string `json:"status"`
}

func effectiveAPIContains(items []effectiveAPIRecord, apiID string, endpointID string) bool {
	for _, item := range items {
		if item.APIID == apiID && item.ProviderEndpoint == endpointID && item.Status == "running" {
			return true
		}
	}
	return false
}

type routeTableResponse struct {
	Routes []routeTableItem `json:"routes"`
}

type routeTableItem struct {
	APIID            string `json:"api_id"`
	ProviderNodeID   string `json:"provider_node_id"`
	ProviderEndpoint string `json:"provider_endpoint"`
	UpstreamBase     string `json:"upstream_base"`
	ProxyEnabled     bool   `json:"proxy_enabled"`
}

func findRoute(routes []routeTableItem, apiID string) *routeTableItem {
	for i := range routes {
		if routes[i].APIID == apiID && routes[i].ProxyEnabled {
			return &routes[i]
		}
	}
	return nil
}

func startOrchestratorAuthStub(cfg smokeConfig) (*http.Server, error) {
	mux := http.NewServeMux()
	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, http.StatusOK, map[string]any{"status": "ok", "service": "orchestrator-auth-stub"})
	})
	mux.HandleFunc("/internal/orchestrator/nodes/", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet || !strings.HasSuffix(r.URL.Path, "/routes") {
			http.NotFound(w, r)
			return
		}
		parts := strings.Split(strings.Trim(r.URL.Path, "/"), "/")
		if len(parts) < 5 || parts[3] != childNodeID {
			http.NotFound(w, r)
			return
		}
		writeJSON(w, http.StatusOK, routeTable(cfg))
	})
	mux.HandleFunc("/auth/permission-check", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			http.NotFound(w, r)
			return
		}
		if strings.TrimSpace(r.Header.Get("Authorization")) == "" {
			writeJSON(w, http.StatusUnauthorized, map[string]any{"code": 401, "msg": "missing authorization"})
			return
		}
		var req struct {
			CallerType    string `json:"caller_type"`
			CallerService string `json:"caller_service"`
			CallerNodeID  string `json:"caller_node_id"`
			APIID         string `json:"api_id"`
			Permission    string `json:"permission"`
		}
		_ = json.NewDecoder(r.Body).Decode(&req)
		cfg.authStubCalls.Record(req.CallerService, req.APIID, req.Permission)
		writeJSON(w, http.StatusOK, map[string]any{
			"code": 0,
			"msg":  "ok",
			"data": map[string]any{"allowed": true},
		})
	})

	server := &http.Server{Handler: mux}
	listener, err := net.Listen("tcp", cfg.orchestrator.host+":"+strconv.Itoa(cfg.orchestrator.port))
	if err != nil {
		return nil, err
	}
	server.Addr = listener.Addr().String()
	go func() {
		if err := server.Serve(listener); err != nil && !errors.Is(err, http.ErrServerClosed) {
			fmt.Fprintf(os.Stderr, "[FAIL] orchestrator/auth stub\nreason: %v\n", err)
		}
	}()
	return server, nil
}

func routeTable(cfg smokeConfig) map[string]any {
	storageBase := cfg.storage.baseURL()
	endpointID := fmt.Sprintf("%s:%d:%s", cfg.storage.host, cfg.storage.port, storageService)
	routes := []map[string]any{
		storageRoute("storage.object.put", []string{http.MethodPut, http.MethodPost}, "storage.object.write", storageBase, endpointID),
		storageRoute("storage.object.get", []string{http.MethodGet}, "storage.object.read", storageBase, endpointID),
		storageRoute("storage.object.head", []string{http.MethodHead, http.MethodGet}, "storage.object.read", storageBase, endpointID),
	}
	return map[string]any{
		"version":      "smoke",
		"generated_at": time.Now().UTC().Format(time.RFC3339Nano),
		"routes":       routes,
		"warnings":     []string{},
		"can_proxy":    true,
	}
}

func storageRoute(apiID string, methods []string, permission string, upstream string, endpointID string) map[string]any {
	return map[string]any{
		"route_id":              storageService + ":" + apiID,
		"api_id":                apiID,
		"node_id":               childNodeID,
		"provider_node_id":      rootNodeID,
		"provider_host_ip":      "127.0.0.1",
		"provider_service_name": storageService,
		"provider_endpoint":     endpointID,
		"visibility_source":     "ancestor:descendants",
		"distance":              1,
		"owner_service_id":      storageService,
		"prefix":                "/api/storage/objects",
		"service_id":            storageService,
		"target_service":        storageService,
		"upstream_base":         upstream,
		"auth_mode":             "service",
		"required_permission":   permission,
		"methods":               methods,
		"enabled":               true,
		"proxy_enabled":         true,
		"priority":              len("/api/storage/objects"),
		"created_from":          "judge-local-smoke-orchestrator-stub",
		"status":                "active",
	}
}

func writeStorageConfig(cfg smokeConfig) (string, error) {
	path := storageConfigPath(cfg)
	minioEndpoint := envDefault("OJOS_REAL_MINIO_ENDPOINT", envDefault("MINIO_ENDPOINT", "127.0.0.1:9000"))
	minioAccessKey := envDefault("OJOS_REAL_MINIO_ACCESS_KEY", envDefault("MINIO_ROOT_USER", "ojos-minio"))
	minioSecretKey := envDefault("OJOS_REAL_MINIO_SECRET_KEY", envDefault("MINIO_ROOT_PASSWORD", "ojos-minio-local"))
	minioUseSSL := envDefault("OJOS_REAL_MINIO_USE_SSL", envDefault("MINIO_USE_SSL", "false"))
	content := fmt.Sprintf(`Name: storage-service-smoke
Host: %s
Port: %d
Storage:
  Backend: %s
  Root: %s
  Buckets:
    - submissions
    - problems
    - judge-artifacts
  MinIO:
    Endpoint: %s
    AccessKey: %s
    SecretKey: %s
    UseSSL: %s
`, cfg.storage.host, cfg.storage.port,
		cfg.storageBackend,
		yamlString(filepath.Join(cfg.workRoot, "storage")),
		yamlString(minioEndpoint),
		yamlString(minioAccessKey),
		yamlString(minioSecretKey),
		minioUseSSL,
	)
	return path, os.WriteFile(path, []byte(content), 0o644)
}

func storageConfigPath(cfg smokeConfig) string {
	return filepath.Join(cfg.workRoot, "config", "storageservice.yaml")
}

func writeGatewayConfig(cfg smokeConfig) (string, error) {
	path := filepath.Join(cfg.workRoot, "config", "gateway.yaml")
	authEndpoint := cfg.orchestrator.baseURL()
	if cfg.authMode == "real" {
		authEndpoint = cfg.auth.baseURL()
	}
	content := fmt.Sprintf(`Name: gateway-smoke
Host: %s
Port: %d
Database:
  Url: ""
Redis:
  Url: %s
Jaeger:
  Endpoint: ""
Jwt:
  Secret: "smoke"
Storage:
  ProblemsRoot: %s
  SubmissionsRoot: %s
Proxy:
  TrustedServices: []
  Routes: []
ServiceStatus:
  ComposeServices: []
InternalAuth:
  Enabled: false
Orchestrator:
  Endpoint: %s
  InternalToken: "smoke"
  NodeID: %s
AuthService:
  Endpoint: %s
`, cfg.gateway.host, cfg.gateway.port,
		yamlString(cfg.redisURL),
		yamlString(filepath.Join(cfg.workRoot, "problems")),
		yamlString(filepath.Join(cfg.workRoot, "submissions")),
		yamlString(cfg.orchestrator.baseURL()),
		yamlString(childNodeID),
		yamlString(authEndpoint),
	)
	return path, os.WriteFile(path, []byte(content), 0o644)
}

func writeAuthConfig(cfg smokeConfig) (string, error) {
	path := filepath.Join(cfg.workRoot, "config", "auth.yaml")
	content := fmt.Sprintf(`Name: auth-service-smoke
Host: %s
Port: %d
Database:
  Url: ""
Jaeger:
  Endpoint: ""
Jwt:
  Secret: "smoke"
  ExpireHours: 24
InternalAuth:
  Token: %s
`, cfg.auth.host, cfg.auth.port, yamlString(serviceToken))
	return path, os.WriteFile(path, []byte(content), 0o644)
}

type processSpec struct {
	name    string
	dir     string
	logPath string
	args    []string
	env     map[string]string
}

type childProcess struct {
	name   string
	cmd    *exec.Cmd
	log    *os.File
	done   chan struct{}
	mu     sync.Mutex
	err    error
	closed sync.Once
}

func startProcess(ctx context.Context, spec processSpec) (*childProcess, error) {
	if len(spec.args) == 0 {
		return nil, errors.New("missing command")
	}
	if err := os.MkdirAll(filepath.Dir(spec.logPath), 0o755); err != nil {
		return nil, err
	}
	logFile, err := os.Create(spec.logPath)
	if err != nil {
		return nil, err
	}
	cmd := exec.CommandContext(ctx, spec.args[0], spec.args[1:]...)
	cmd.Dir = spec.dir
	cmd.Env = processEnv(spec.env)
	cmd.Stdout = logFile
	cmd.Stderr = logFile
	proc := &childProcess{
		name: spec.name,
		cmd:  cmd,
		log:  logFile,
		done: make(chan struct{}),
	}
	if err := cmd.Start(); err != nil {
		_ = logFile.Close()
		return nil, err
	}
	go func() {
		err := cmd.Wait()
		proc.mu.Lock()
		proc.err = err
		proc.mu.Unlock()
		close(proc.done)
		proc.closeLog()
	}()
	return proc, nil
}

func (p *childProcess) Wait(timeout time.Duration) error {
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case <-p.done:
		return p.waitErr()
	case <-timer.C:
		p.killTree()
		<-p.done
		return fmt.Errorf("%s did not exit within %s", p.name, timeout)
	}
}

func (p *childProcess) Stop() {
	select {
	case <-p.done:
		p.closeLog()
		return
	default:
	}
	p.killTree()
	select {
	case <-p.done:
	case <-time.After(3 * time.Second):
	}
	p.closeLog()
}

func (p *childProcess) killTree() {
	if p.cmd == nil || p.cmd.Process == nil {
		return
	}
	if runtime.GOOS == "windows" {
		_ = exec.Command("taskkill", "/PID", strconv.Itoa(p.cmd.Process.Pid), "/T", "/F").Run()
		return
	}
	_ = p.cmd.Process.Kill()
}

func (p *childProcess) waitErr() error {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.err == nil {
		return nil
	}
	if exit, ok := p.err.(*exec.ExitError); ok && exit.Success() {
		return nil
	}
	return p.err
}

func (p *childProcess) exitedErr() error {
	select {
	case <-p.done:
		if err := p.waitErr(); err != nil {
			return fmt.Errorf("%s exited: %w", p.name, err)
		}
		return fmt.Errorf("%s exited before health check completed", p.name)
	default:
		return nil
	}
}

func (p *childProcess) closeLog() {
	p.closed.Do(func() {
		if p.log != nil {
			_ = p.log.Close()
		}
	})
}

func createSubmission(ctx context.Context, cfg smokeConfig) (int64, error) {
	body := map[string]any{
		"problem_id": int64(1001),
		"language":   "cpp17",
		"code":       submissionSourceCode,
	}
	var resp struct {
		SubmissionID int64  `json:"submission_id"`
		Status       string `json:"status"`
	}
	if err := doJSON(ctx, http.MethodPost, cfg.judgeAPI.baseURL()+"/judge/submissions", body, &resp); err != nil {
		return 0, err
	}
	if resp.SubmissionID <= 0 {
		return 0, fmt.Errorf("invalid submission id: %d", resp.SubmissionID)
	}
	return resp.SubmissionID, nil
}

func waitSubmissionStatus(ctx context.Context, cfg smokeConfig, submissionID int64) (string, error) {
	deadline := time.Now().Add(20 * time.Second)
	var last string
	for time.Now().Before(deadline) {
		var resp struct {
			Status string `json:"status"`
		}
		err := doJSON(ctx, http.MethodGet, fmt.Sprintf("%s/judge/submissions/%d", cfg.judgeAPI.baseURL(), submissionID), nil, &resp)
		if err == nil {
			last = resp.Status
			if isTerminalStatus(resp.Status) {
				return resp.Status, nil
			}
		}
		if wait(ctx, 300*time.Millisecond) != nil {
			return "", ctx.Err()
		}
	}
	return "", fmt.Errorf("submission %d did not finish, last status=%q", submissionID, last)
}

func querySubmissionCases(ctx context.Context, cfg smokeConfig, submissionID int64) error {
	var resp struct {
		Cases []struct {
			Status string `json:"status"`
		} `json:"cases"`
	}
	if err := doJSON(ctx, http.MethodGet, fmt.Sprintf("%s/judge/submissions/%d/cases", cfg.judgeAPI.baseURL(), submissionID), nil, &resp); err != nil {
		return err
	}
	if len(resp.Cases) == 0 {
		return errors.New("result cases are empty")
	}
	if resp.Cases[0].Status != "ACCEPTED" {
		return fmt.Errorf("unexpected case status %q", resp.Cases[0].Status)
	}
	return nil
}

func doJSON(ctx context.Context, method string, target string, body any, out any) error {
	return doJSONWithHeaders(ctx, method, target, body, map[string]string{
		"X-Auth-Verified": "true",
		"X-User-Id":       "7",
		"X-Username":      "smoke",
		"X-Roles":         "user",
	}, out)
}

func doJSONWithHeaders(ctx context.Context, method string, target string, body any, headers map[string]string, out any) error {
	var reader io.Reader
	if body != nil {
		data, err := json.Marshal(body)
		if err != nil {
			return err
		}
		reader = bytes.NewReader(data)
	}
	req, err := http.NewRequestWithContext(ctx, method, target, reader)
	if err != nil {
		return err
	}
	for key, value := range headers {
		if strings.TrimSpace(value) != "" {
			req.Header.Set(key, value)
		}
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := smokeHTTP.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	data, _ := io.ReadAll(io.LimitReader(resp.Body, 1024*1024))
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("%s returned %s: %s", target, resp.Status, strings.TrimSpace(string(data)))
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(data, out)
}

func doStatus(ctx context.Context, method string, target string, body any, headers map[string]string) (int, []byte, error) {
	var reader io.Reader
	contentType := ""
	if body != nil {
		switch value := body.(type) {
		case io.Reader:
			reader = value
		case string:
			reader = strings.NewReader(value)
			contentType = "text/plain; charset=utf-8"
		case []byte:
			reader = bytes.NewReader(value)
			contentType = "application/octet-stream"
		default:
			data, err := json.Marshal(body)
			if err != nil {
				return 0, nil, err
			}
			reader = bytes.NewReader(data)
			contentType = "application/json"
		}
	}
	req, err := http.NewRequestWithContext(ctx, method, target, reader)
	if err != nil {
		return 0, nil, err
	}
	for key, value := range headers {
		if strings.TrimSpace(value) != "" {
			req.Header.Set(key, value)
		}
	}
	if contentType != "" {
		req.Header.Set("Content-Type", contentType)
	}
	resp, err := smokeHTTP.Do(req)
	if err != nil {
		return 0, nil, err
	}
	defer resp.Body.Close()
	data, _ := io.ReadAll(io.LimitReader(resp.Body, 1024*1024))
	return resp.StatusCode, data, nil
}

func findTaskEntry(ctx context.Context, client *redis.Client, submissionID int64) (string, error) {
	wantSubmission := strconv.FormatInt(submissionID, 10)
	wantTask := "sub-" + wantSubmission
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		entries, err := client.XRange(ctx, taskStream, "-", "+").Result()
		if err != nil {
			return "", err
		}
		for _, entry := range entries {
			if fmt.Sprint(entry.Values["submission_id"]) == wantSubmission && fmt.Sprint(entry.Values["task_id"]) == wantTask {
				return entry.ID, nil
			}
		}
		if wait(ctx, 200*time.Millisecond) != nil {
			return "", ctx.Err()
		}
	}
	return "", fmt.Errorf("task entry not found for submission %d", submissionID)
}

func findResultEntry(ctx context.Context, client *redis.Client, submissionID int64) (string, string, error) {
	wantSubmission := strconv.FormatInt(submissionID, 10)
	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		entries, err := client.XRange(ctx, resultStream, "-", "+").Result()
		if err != nil {
			return "", "", err
		}
		for _, entry := range entries {
			if fmt.Sprint(entry.Values["submission_id"]) == wantSubmission {
				return entry.ID, fmt.Sprint(entry.Values["status"]), nil
			}
		}
		if wait(ctx, 300*time.Millisecond) != nil {
			return "", "", ctx.Err()
		}
	}
	return "", "", fmt.Errorf("result entry not found for submission %d", submissionID)
}

func pendingCount(ctx context.Context, client *redis.Client) (int64, error) {
	value, err := client.Do(ctx, "XPENDING", taskStream, consumerGroup).Result()
	if err != nil {
		return 0, err
	}
	items, ok := value.([]any)
	if !ok || len(items) == 0 {
		return 0, fmt.Errorf("unexpected XPENDING response: %#v", value)
	}
	switch count := items[0].(type) {
	case int64:
		return count, nil
	case uint64:
		return int64(count), nil
	default:
		return 0, fmt.Errorf("unexpected XPENDING count: %#v", count)
	}
}

func verifyQueueStatusAPI(ctx context.Context, cfg smokeConfig) error {
	var resp struct {
		TaskStream        string `json:"task_stream"`
		ResultStream      string `json:"result_stream"`
		Group             string `json:"group"`
		PendingCount      int64  `json:"pending_count"`
		ConsumerLag       int64  `json:"consumer_lag"`
		Lag               int64  `json:"lag"`
		ConsumerCount     int64  `json:"consumer_count"`
		LastID            string `json:"last_id"`
		ResultLastID      string `json:"result_last_id"`
		PendingOldestIdle int64  `json:"pending_oldest_idle_ms"`
		RedisStatus       string `json:"redis_status"`
		Consumers         []struct {
			Name       string `json:"name"`
			Pending    int64  `json:"pending"`
			IdleMs     int64  `json:"idle_ms"`
			InactiveMs int64  `json:"inactive_ms"`
		} `json:"consumers"`
	}
	headers := map[string]string{
		"X-Auth-Verified": "true",
		"X-User-Id":       "1",
		"X-Username":      "smoke-admin",
		"X-Roles":         "admin",
	}
	if err := doJSONWithHeaders(ctx, http.MethodGet, cfg.judgeAPI.baseURL()+"/judge/admin/queue/status", nil, headers, &resp); err != nil {
		return fail("redis queue status API returned pending/lag", err)
	}
	if resp.TaskStream != taskStream || resp.ResultStream != resultStream || resp.Group != consumerGroup {
		return fail("redis queue status API returned pending/lag", fmt.Errorf("unexpected stream identity: %#v", resp))
	}
	if resp.RedisStatus != "ok" && resp.RedisStatus != "partial" {
		return fail("redis queue status API returned pending/lag", fmt.Errorf("unexpected redis_status %q", resp.RedisStatus))
	}
	if resp.PendingCount != 0 {
		return fail("redis queue status API returned pending/lag", fmt.Errorf("expected zero pending after ack, got %d", resp.PendingCount))
	}
	lag := resp.Lag
	if lag < 0 {
		lag = resp.ConsumerLag
	}
	if lag < 0 {
		return fail("redis queue status API returned pending/lag", fmt.Errorf("lag was not populated: %#v", resp))
	}
	if resp.ConsumerCount == 0 || len(resp.Consumers) == 0 {
		return fail("redis queue status API returned pending/lag", fmt.Errorf("expected at least one consumer: %#v", resp))
	}
	if strings.TrimSpace(resp.LastID) == "" || strings.TrimSpace(resp.ResultLastID) == "" {
		return fail("redis queue status API returned pending/lag", fmt.Errorf("expected stream last ids: %#v", resp))
	}
	for _, consumer := range resp.Consumers {
		if strings.TrimSpace(consumer.Name) == "" || consumer.IdleMs < 0 || consumer.InactiveMs < 0 {
			return fail("redis queue status API returned pending/lag", fmt.Errorf("consumer idle fields missing: %#v", resp.Consumers))
		}
	}
	ok("redis queue status API returned pending=%d lag=%d consumers=%d idle_ms=%d", resp.PendingCount, lag, len(resp.Consumers), resp.Consumers[0].IdleMs)
	return nil
}

func connectRedis(ctx context.Context, redisURL string) (*redis.Client, error) {
	options, err := redis.ParseURL(redisURL)
	if err != nil {
		return nil, err
	}
	client := redis.NewClient(options)
	if err := client.Ping(ctx).Err(); err != nil {
		_ = client.Close()
		return nil, err
	}
	return client, nil
}

func waitHealth(ctx context.Context, target string) error {
	deadline := time.Now().Add(25 * time.Second)
	var last error
	for time.Now().Before(deadline) {
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
		if err != nil {
			return err
		}
		resp, err := smokeHTTP.Do(req)
		if err == nil {
			_ = resp.Body.Close()
			if resp.StatusCode >= 200 && resp.StatusCode < 300 {
				return nil
			}
			last = fmt.Errorf("status %s", resp.Status)
		} else {
			last = err
		}
		if wait(ctx, 300*time.Millisecond) != nil {
			return ctx.Err()
		}
	}
	if last == nil {
		last = errors.New("health check timed out")
	}
	return last
}

func waitProcessHealth(ctx context.Context, proc *childProcess, target string) error {
	deadline := time.Now().Add(25 * time.Second)
	var last error
	for time.Now().Before(deadline) {
		if err := proc.exitedErr(); err != nil {
			return err
		}
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
		if err != nil {
			return err
		}
		resp, err := smokeHTTP.Do(req)
		if err == nil {
			_ = resp.Body.Close()
			if resp.StatusCode >= 200 && resp.StatusCode < 300 {
				return nil
			}
			last = fmt.Errorf("status %s", resp.Status)
		} else {
			last = err
		}
		if wait(ctx, 300*time.Millisecond) != nil {
			return ctx.Err()
		}
	}
	if err := proc.exitedErr(); err != nil {
		return err
	}
	if last == nil {
		last = errors.New("health check timed out")
	}
	return last
}

func waitStorageUnavailable(ctx context.Context, target string) error {
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
		if err != nil {
			return err
		}
		resp, err := smokeHTTP.Do(req)
		if err != nil {
			return nil
		}
		_ = resp.Body.Close()
		if resp.StatusCode >= 500 {
			return nil
		}
		if wait(ctx, 300*time.Millisecond) != nil {
			return ctx.Err()
		}
	}
	return errors.New("storage-service still responds after rollback")
}

func waitFile(ctx context.Context, path string) error {
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if stat, err := os.Stat(path); err == nil && stat.Size() >= 0 {
			return nil
		}
		if wait(ctx, 200*time.Millisecond) != nil {
			return ctx.Err()
		}
	}
	return fmt.Errorf("file not found: %s", path)
}

func waitLogContains(ctx context.Context, path string, needle string) error {
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		data, err := os.ReadFile(path)
		if err == nil && bytes.Contains(data, []byte(needle)) {
			return nil
		}
		if wait(ctx, 200*time.Millisecond) != nil {
			return ctx.Err()
		}
	}
	return fmt.Errorf("log %s does not contain %q", path, needle)
}

func wait(ctx context.Context, duration time.Duration) error {
	timer := time.NewTimer(duration)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return nil
	}
}

func prepareWorkRoot(root string) error {
	if strings.TrimSpace(root) == "" {
		return errors.New("work root is empty")
	}
	if err := os.RemoveAll(root); err != nil {
		return err
	}
	for _, dir := range []string{
		filepath.Join(root, "config"),
		filepath.Join(root, "logs"),
		filepath.Join(root, "storage"),
		filepath.Join(root, "submissions"),
		filepath.Join(root, "problems"),
		filepath.Join(root, "worker-work"),
		filepath.Join(root, "worker-cache"),
	} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return err
		}
	}
	return nil
}

func findRepoRoot() (string, error) {
	dir, err := os.Getwd()
	if err != nil {
		return "", err
	}
	for {
		if _, err := os.Stat(filepath.Join(dir, "deploy", "compose", "docker-compose.yml")); err == nil {
			return dir, nil
		}
		next := filepath.Dir(dir)
		if next == dir {
			return "", errors.New("could not find repo root")
		}
		dir = next
	}
}

func allocateSmokeEndpoints() (endpoint, endpoint, endpoint, endpoint, endpoint, error) {
	orchestrator, err := freeEndpoint()
	if err != nil {
		return endpoint{}, endpoint{}, endpoint{}, endpoint{}, endpoint{}, err
	}
	auth, err := freeEndpoint()
	if err != nil {
		return endpoint{}, endpoint{}, endpoint{}, endpoint{}, endpoint{}, err
	}
	storage, err := freeEndpoint()
	if err != nil {
		return endpoint{}, endpoint{}, endpoint{}, endpoint{}, endpoint{}, err
	}
	gateway, err := freeEndpoint()
	if err != nil {
		return endpoint{}, endpoint{}, endpoint{}, endpoint{}, endpoint{}, err
	}
	judgeAPI, err := freeEndpoint()
	if err != nil {
		return endpoint{}, endpoint{}, endpoint{}, endpoint{}, endpoint{}, err
	}
	return orchestrator, auth, storage, gateway, judgeAPI, nil
}

func freeEndpoint() (endpoint, error) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return endpoint{}, err
	}
	defer listener.Close()
	addr, ok := listener.Addr().(*net.TCPAddr)
	if !ok {
		return endpoint{}, fmt.Errorf("unexpected listener addr: %s", listener.Addr())
	}
	return endpoint{host: "127.0.0.1", port: addr.Port}, nil
}

func cleanupStaleSmokeProcesses(workRoot string) error {
	if runtime.GOOS != "windows" {
		return nil
	}
	workRoot = strings.TrimSpace(workRoot)
	if workRoot == "" {
		return nil
	}
	script := fmt.Sprintf(
		`Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*%s*' -and ($_.Name -eq 'go.exe' -or $_.Name -eq 'storageservice.exe' -or $_.Name -eq 'ojos-storage-service.exe' -or $_.Name -eq 'ojos-gateway.exe' -or $_.Name -eq 'smoke-server.exe' -or $_.Name -eq 'judge-worker.exe' -or $_.Name -eq 'ojos-orchestrator-daemon.exe' -or $_.Name -eq 'ojos-auth-service.exe') } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }`,
		strings.ReplaceAll(workRoot, "'", "''"),
	)
	cmd := exec.Command("powershell", "-NoProfile", "-Command", script)
	return cmd.Run()
}

func processEnv(overrides map[string]string) []string {
	out := os.Environ()
	for key, value := range overrides {
		out = setEnv(out, key, value)
	}
	return out
}

func noProxyEnv(overrides map[string]string) map[string]string {
	out := map[string]string{
		"NO_PROXY":    "127.0.0.1,localhost",
		"no_proxy":    "127.0.0.1,localhost",
		"HTTP_PROXY":  "",
		"HTTPS_PROXY": "",
		"ALL_PROXY":   "",
		"http_proxy":  "",
		"https_proxy": "",
		"all_proxy":   "",
	}
	for key, value := range overrides {
		out[key] = value
	}
	return out
}

func normalizeSmokeMode(value string) string {
	value = strings.ToLower(strings.TrimSpace(value))
	switch value {
	case "", "stub":
		return "stub"
	case "real":
		return "real"
	default:
		return value
	}
}

func normalizeMatrixMode(value string) string {
	value = strings.ToLower(strings.TrimSpace(value))
	switch value {
	case "":
		return ""
	case "beta-local":
		return "beta-local"
	default:
		return value
	}
}

func normalizeStorageBackend(value string) string {
	value = strings.ToLower(strings.TrimSpace(value))
	switch value {
	case "", "local":
		return "local"
	case "minio":
		return "minio"
	default:
		return value
	}
}

func normalizeInstallMode(value string, controlPlaneMode string) string {
	value = strings.ToLower(strings.TrimSpace(value))
	if value == "" {
		if controlPlaneMode == "real" {
			return "release-install"
		}
		return "seed"
	}
	switch value {
	case "seed", "release-install":
		return value
	default:
		return value
	}
}

func setEnv(env []string, key string, value string) []string {
	prefix := strings.ToLower(key) + "="
	out := env[:0]
	for _, item := range env {
		if strings.HasPrefix(strings.ToLower(item), prefix) {
			continue
		}
		out = append(out, item)
	}
	return append(out, key+"="+value)
}

func normalizeRedisURL(raw string) string {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return "redis://127.0.0.1:6379/0"
	}
	if strings.Contains(raw, "://") {
		return raw
	}
	return "redis://" + raw
}

func yamlString(value string) string {
	return strconv.Quote(filepath.ToSlash(value))
}

func isTerminalStatus(status string) bool {
	switch strings.ToUpper(strings.TrimSpace(status)) {
	case "ACCEPTED", "WRONG_ANSWER", "COMPILE_ERROR", "RUNTIME_ERROR", "TIME_LIMIT_EXCEEDED", "MEMORY_LIMIT_EXCEEDED", "SYSTEM_ERROR", "FINISHED":
		return true
	default:
		return false
	}
}

func writeJSON(w http.ResponseWriter, status int, body any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(body)
}

func shutdownHTTPServer(server *http.Server) {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	_ = server.Shutdown(ctx)
}

func fail(step string, err error) error {
	if err == nil {
		err = errors.New("unknown error")
	}
	return stepError{step: step, err: err}
}

func ok(format string, args ...any) {
	fmt.Printf("[OK] "+format+"\n", args...)
}

func skip(format string, args ...any) {
	fmt.Printf("[SKIP] "+format+"\n", args...)
}

func printBetaMatrix(parent context.Context, cfg smokeConfig) {
	ok("beta-local matrix: release.install-driven judge smoke completed")
	ok("orchestrator real")
	ok("nodes created through API")
	ok("auth real")
	ok("service identity allow/deny")
	skip("release package install in smoke: source tree release.yaml")
	ok("release.install storage-service")
	ok("release.install service_start storage-service local-process")
	ok("gateway reload orchestrator-driven")
	ok("storage backend %s", cfg.storageBackend)
	ok("Redis task/result")
	ok("queue pending/lag")
	ok("judge-worker fake runner")
	ok("result ACCEPTED")
	printComposeMatrixStatus(parent, cfg)
	if cfg.storageBackend == "minio" {
		ok("MinIO beta smoke: storage-service backend=minio")
	} else {
		skip("MinIO beta smoke: storage backend is local")
	}
	printMinIOMatrixStatus(parent, cfg)
	printNsjailMatrixStatus(parent)
}

func printComposeMatrixStatus(parent context.Context, cfg smokeConfig) {
	services := []string{
		"redis",
		"auth-db",
		"problem-db",
		"judge-db",
		"user-db",
		"orchestrator-db",
		"auth-service",
		"storage-service",
		"gateway",
		"judge-api",
	}
	statuses, err := composeServiceStatuses(parent, cfg)
	if err != nil {
		skip("compose full up: %v", err)
		return
	}
	missing := make([]string, 0)
	for _, service := range services {
		status, found := statuses[service]
		if !found {
			missing = append(missing, service+" missing")
			continue
		}
		if strings.ToLower(strings.TrimSpace(status.State)) != "running" {
			missing = append(missing, service+" state="+status.State)
			continue
		}
		if status.Health != "" && strings.ToLower(strings.TrimSpace(status.Health)) != "healthy" {
			missing = append(missing, service+" health="+status.Health)
		}
	}
	if len(missing) > 0 {
		skip("compose full up: %s", strings.Join(missing, ", "))
		return
	}
	ok("compose full up: required beta services running")
}

type composePSStatus struct {
	Service string `json:"Service"`
	State   string `json:"State"`
	Health  string `json:"Health"`
	Status  string `json:"Status"`
}

func composeServiceStatuses(parent context.Context, cfg smokeConfig) (map[string]composePSStatus, error) {
	composePath := filepath.Join(cfg.repoRoot, "deploy", "compose", "docker-compose.yml")
	ctx, cancel := matrixContext(parent, 12*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, "docker", "compose", "-f", composePath, "ps", "--format", "json")
	cmd.Dir = cfg.repoRoot
	out, err := cmd.CombinedOutput()
	if err != nil {
		return nil, fmt.Errorf("docker compose ps failed: %s", oneLine(out, err))
	}
	statuses := map[string]composePSStatus{}
	dec := json.NewDecoder(bytes.NewReader(out))
	for {
		var status composePSStatus
		if err := dec.Decode(&status); err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			return nil, fmt.Errorf("parse docker compose ps JSON: %w", err)
		}
		if status.Service != "" {
			statuses[status.Service] = status
		}
	}
	return statuses, nil
}

func printMinIOMatrixStatus(parent context.Context, cfg smokeConfig) {
	endpoint := strings.TrimSpace(os.Getenv("OJOS_REAL_MINIO_ENDPOINT"))
	accessKey := strings.TrimSpace(os.Getenv("OJOS_REAL_MINIO_ACCESS_KEY"))
	secretKey := strings.TrimSpace(os.Getenv("OJOS_REAL_MINIO_SECRET_KEY"))
	if endpoint == "" && tcpReachable("127.0.0.1:9000", 800*time.Millisecond) {
		endpoint = "127.0.0.1:9000"
		accessKey = envDefault("MINIO_ROOT_USER", "ojos-minio")
		secretKey = envDefault("MINIO_ROOT_PASSWORD", "ojos-minio-local")
	}
	if endpoint == "" || accessKey == "" || secretKey == "" {
		skip("MinIO live: no reachable MinIO endpoint configured")
		return
	}
	ctx, cancel := matrixContext(parent, 90*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, "go", "test", "./...", "-run", "TestRealMinIO", "-count=1")
	cmd.Dir = filepath.Join(cfg.repoRoot, "services", "storage-service")
	cmd.Env = processEnv(noProxyEnv(map[string]string{
		"OJOS_REAL_MINIO_ENDPOINT":   endpoint,
		"OJOS_REAL_MINIO_ACCESS_KEY": accessKey,
		"OJOS_REAL_MINIO_SECRET_KEY": secretKey,
		"OJOS_REAL_MINIO_USE_SSL":    envDefault("OJOS_REAL_MINIO_USE_SSL", "false"),
	}))
	out, err := cmd.CombinedOutput()
	if err != nil {
		skip("MinIO live: TestRealMinIO did not pass: %s", oneLine(out, err))
		return
	}
	ok("MinIO live: TestRealMinIO passed")
}

func printNsjailMatrixStatus(parent context.Context) {
	if runtime.GOOS != "linux" {
		skip("nsjail live: current OS is %s", runtime.GOOS)
		return
	}
	if _, err := exec.LookPath("nsjail"); err != nil {
		skip("nsjail live: nsjail unavailable on PATH")
		return
	}
	ctx, cancel := matrixContext(parent, 5*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, "nsjail", "--version")
	if out, err := cmd.CombinedOutput(); err != nil {
		skip("nsjail live: nsjail version probe failed: %s", oneLine(out, err))
		return
	}
	ok("nsjail live: nsjail binary available; full verdict matrix still deferred")
}

func matrixContext(parent context.Context, timeout time.Duration) (context.Context, context.CancelFunc) {
	if parent == nil || parent.Err() != nil {
		return context.WithTimeout(context.Background(), timeout)
	}
	return context.WithTimeout(parent, timeout)
}

func tcpReachable(address string, timeout time.Duration) bool {
	conn, err := net.DialTimeout("tcp", address, timeout)
	if err != nil {
		return false
	}
	_ = conn.Close()
	return true
}

func oneLine(output []byte, err error) string {
	text := strings.TrimSpace(string(output))
	text = strings.ReplaceAll(text, "\r", " ")
	text = strings.ReplaceAll(text, "\n", " ")
	text = strings.Join(strings.Fields(text), " ")
	if text == "" {
		text = err.Error()
	}
	if len(text) > 240 {
		return text[:240] + "..."
	}
	return text
}

func envDefault(key string, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(key)); value != "" {
		return value
	}
	return fallback
}

func printLastLogs(w io.Writer, logDir string) {
	for _, name := range []string{"orchestrator.log", "auth-service.log", "storage-service.log", "gateway.log", "judge-api.log", "judge-worker.log"} {
		path := filepath.Join(logDir, name)
		text := lastLines(path, 30)
		if strings.TrimSpace(text) == "" {
			continue
		}
		fmt.Fprintf(w, "\nlast %s:\n%s\n", name, text)
	}
}

func lastLines(path string, n int) string {
	data, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	lines := bytes.Split(bytes.TrimRight(data, "\r\n"), []byte{'\n'})
	if len(lines) > n {
		lines = lines[len(lines)-n:]
	}
	return string(bytes.Join(lines, []byte{'\n'}))
}

type authCallRecorder struct {
	mu    sync.Mutex
	calls map[string]int
}

func newAuthCallRecorder() *authCallRecorder {
	return &authCallRecorder{calls: map[string]int{}}
}

func (r *authCallRecorder) Record(callerService string, apiID string, permission string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	key := strings.TrimSpace(callerService) + " " + strings.TrimSpace(apiID) + " " + strings.TrimSpace(permission)
	r.calls[key]++
}

func (r *authCallRecorder) HasCaller(callerService string) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	prefix := strings.TrimSpace(callerService) + " "
	for key := range r.calls {
		if strings.HasPrefix(key, prefix) {
			return true
		}
	}
	return false
}

func (r *authCallRecorder) Snapshot() map[string]int {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make(map[string]int, len(r.calls))
	for key, value := range r.calls {
		out[key] = value
	}
	return out
}
