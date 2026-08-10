package main

import (
	"archive/zip"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
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
	problemService       = "problem-service"
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
	releaseSource    string
	storagePackage   string
	judgeAPIPackage  string
	taskStream       string
	resultStream     string
	serviceToken     string
	workerToken      string
	gatewayAdminJWT  string
	composeUserJWT   string
	composeProblemID int64
	orchestrator     endpoint
	auth             endpoint
	authServiceStart endpoint
	storage          endpoint
	storageProvider  endpoint
	gateway          endpoint
	gatewayStart     endpoint
	judgeAPI         endpoint
	timeout          time.Duration
	cleanStreams     bool
	lastTaskID       string
	lastResultID     string
	releasePackages  map[string]releasePackageInfo
	authStubCalls    *authCallRecorder
}

type endpoint struct {
	host string
	port int
}

type releasePackageInfo struct {
	path      string
	sourceURL string
	checksum  string
}

func (e endpoint) baseURL() string {
	return fmt.Sprintf("http://%s:%d", e.host, e.port)
}

func storageProviderEndpoint(cfg smokeConfig) endpoint {
	if strings.TrimSpace(cfg.storageProvider.host) != "" && cfg.storageProvider.port > 0 {
		return cfg.storageProvider
	}
	return cfg.storage
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
		redisURL        = flag.String("redis", envDefault("OJOS_REAL_REDIS_URL", envDefault("REDIS_URL", "redis://127.0.0.1:6379/0")), "Redis URL for live smoke")
		controlPlane    = flag.String("control-plane", envDefault("OJOS_SMOKE_CONTROL_PLANE", "stub"), "control plane mode: stub or real")
		authMode        = flag.String("auth", envDefault("OJOS_SMOKE_AUTH", "stub"), "auth mode: stub or real")
		installMode     = flag.String("install-mode", envDefault("OJOS_SMOKE_INSTALL_MODE", ""), "install mode: seed or release-install")
		mode            = flag.String("mode", envDefault("OJOS_JUDGE_SMOKE_MODE", ""), "smoke matrix mode: beta-local or compose")
		storageBackend  = flag.String("storage-backend", envDefault("OJOS_SMOKE_STORAGE_BACKEND", "local"), "storage backend for storage-service: local or minio")
		releaseSource   = flag.String("release-source", envDefault("OJOS_SMOKE_RELEASE_SOURCE", ""), "release source for release.install: tree or package")
		storagePackage  = flag.String("storage-release-package", envDefault("OJOS_STORAGE_RELEASE_PACKAGE", ""), "optional storage-service release package zip path")
		judgeAPIPackage = flag.String("judge-api-release-package", envDefault("OJOS_JUDGE_API_RELEASE_PACKAGE", ""), "optional judge-api release package zip path")
		workRoot        = flag.String("work-root", "", "smoke workspace; defaults to <repo>/.smoke/judge-local")
		timeout         = flag.Duration("timeout", 90*time.Second, "overall smoke timeout")
		cleanStreams    = flag.Bool("clean-streams", true, "delete judge task/result stream keys before the smoke")
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
	if normalizedMatrixMode != "" && normalizedMatrixMode != "beta-local" && normalizedMatrixMode != "compose" {
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
	if normalizedMatrixMode == "compose" {
		*controlPlane = "real"
		*authMode = "real"
		*installMode = "release-install"
		if *timeout < 300*time.Second {
			*timeout = 300 * time.Second
		}
	}
	normalizedStorageBackend := normalizeStorageBackend(*storageBackend)
	if normalizedStorageBackend != "local" && normalizedStorageBackend != "minio" {
		fmt.Fprintf(os.Stderr, "[FAIL] storage backend\nreason: unsupported storage backend %q\n", normalizedStorageBackend)
		os.Exit(1)
	}
	normalizedReleaseSource := normalizeReleaseSource(*releaseSource)
	if normalizedReleaseSource == "" {
		if normalizedMatrixMode == "beta-local" {
			normalizedReleaseSource = "package"
		} else {
			normalizedReleaseSource = "tree"
		}
	}
	if normalizedReleaseSource != "tree" && normalizedReleaseSource != "package" {
		fmt.Fprintf(os.Stderr, "[FAIL] release source\nreason: unsupported release source %q\n", normalizedReleaseSource)
		os.Exit(1)
	}
	smokeTaskStream := strings.TrimSpace(envDefault("OJOS_SMOKE_JUDGE_TASK_STREAM", ""))
	smokeResultStream := strings.TrimSpace(envDefault("OJOS_SMOKE_JUDGE_RESULT_STREAM", ""))
	if smokeTaskStream == "" {
		if normalizedMatrixMode == "beta-local" {
			smokeTaskStream = taskStream + ":beta-local"
		} else {
			smokeTaskStream = taskStream
		}
	}
	if smokeResultStream == "" {
		if normalizedMatrixMode == "beta-local" {
			smokeResultStream = resultStream + ":beta-local"
		} else {
			smokeResultStream = resultStream
		}
	}
	orchestratorEndpoint, authEndpoint, storageEndpoint, gatewayEndpoint, judgeAPIEndpoint, err := allocateSmokeEndpoints()
	if err != nil {
		fmt.Fprintf(os.Stderr, "[FAIL] allocate smoke ports\nreason: %v\n", err)
		os.Exit(1)
	}
	authServiceStartEndpoint, err := freeEndpoint()
	if err != nil {
		fmt.Fprintf(os.Stderr, "[FAIL] allocate smoke ports\nreason: %v\n", err)
		os.Exit(1)
	}
	gatewayStartEndpoint, err := freeEndpoint()
	if err != nil {
		fmt.Fprintf(os.Stderr, "[FAIL] allocate smoke ports\nreason: %v\n", err)
		os.Exit(1)
	}
	if normalizedMatrixMode == "compose" {
		orchestratorEndpoint = endpoint{host: "127.0.0.1", port: 8090}
		authEndpoint = endpoint{host: "127.0.0.1", port: 8081}
		storageEndpoint = endpoint{host: "127.0.0.1", port: 8085}
		gatewayEndpoint = endpoint{host: "127.0.0.1", port: 8080}
		judgeAPIEndpoint = endpoint{host: "127.0.0.1", port: 8082}
		authServiceStartEndpoint = authEndpoint
		gatewayStartEndpoint = gatewayEndpoint
	}
	smokeServiceToken := serviceToken
	smokeWorkerToken := workerToken
	if normalizedMatrixMode == "compose" {
		smokeServiceToken = envDefault("AUTH_INTERNAL_TOKEN", "ojos-local-internal")
		smokeWorkerToken = envDefault("OJOS_WORKER_TOKEN", "ojos-local-worker")
	}
	cfg := smokeConfig{
		repoRoot:         repoRoot,
		workRoot:         *workRoot,
		mode:             normalizedMatrixMode,
		redisURL:         normalizeRedisURL(*redisURL),
		controlPlaneMode: normalizeSmokeMode(*controlPlane),
		authMode:         normalizeSmokeMode(*authMode),
		storageBackend:   normalizedStorageBackend,
		releaseSource:    normalizedReleaseSource,
		storagePackage:   *storagePackage,
		judgeAPIPackage:  *judgeAPIPackage,
		taskStream:       smokeTaskStream,
		resultStream:     smokeResultStream,
		serviceToken:     smokeServiceToken,
		workerToken:      smokeWorkerToken,
		orchestrator:     orchestratorEndpoint,
		auth:             authEndpoint,
		authServiceStart: authServiceStartEndpoint,
		storage:          storageEndpoint,
		storageProvider:  storageEndpoint,
		gateway:          gatewayEndpoint,
		gatewayStart:     gatewayStartEndpoint,
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
	gatewayJWTSecret := "smoke"
	if normalizedMatrixMode == "compose" {
		gatewayJWTSecret = envDefault("JWT_SECRET", "ojos-local-jwt")
	}
	adminJWT, err := sharedjwt.Generate(gatewayJWTSecret, 1, "smoke-admin", []string{"admin"}, 24)
	if err != nil {
		fmt.Fprintf(os.Stderr, "[FAIL] gateway admin token\nreason: %v\n", err)
		os.Exit(1)
	}
	cfg.gatewayAdminJWT = adminJWT

	ctx, cancel := context.WithTimeout(context.Background(), cfg.timeout)
	defer cancel()

	var runErr error
	if cfg.mode == "compose" {
		runErr = runCompose(ctx, cfg)
	} else {
		runErr = run(ctx, cfg)
	}
	if runErr != nil {
		var step stepError
		if errors.As(runErr, &step) {
			fmt.Fprintf(os.Stderr, "[FAIL] %s\nreason: %v\n", step.step, step.err)
		} else {
			fmt.Fprintf(os.Stderr, "[FAIL] smoke\nreason: %v\n", runErr)
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
	if cfg.releaseSource == "package" {
		packages, err := prepareReleasePackages(cfg)
		if err != nil {
			return fail("release package prepared", err)
		}
		cfg.releasePackages = packages
	}
	storageCfg, err := writeStorageConfig(cfg)
	if err != nil {
		return fail("storage-service config", err)
	}
	if _, err := writeAuthConfigForEndpoint(cfg, serviceStartAuthConfigPath(cfg), cfg.authServiceStart, "auth-service-local-process-smoke"); err != nil {
		return fail("auth-service local-process config", err)
	}
	if _, err := writeGatewayConfigForEndpoint(cfg, serviceStartGatewayConfigPath(cfg), cfg.gatewayStart, "gateway-local-process-smoke"); err != nil {
		return fail("gateway local-process config", err)
	}

	redisClient, err := connectRedis(ctx, cfg.redisURL)
	if err != nil {
		return fail("redis connected", err)
	}
	defer redisClient.Close()
	if cfg.cleanStreams {
		if err := redisClient.Del(ctx, cfg.taskStream, cfg.resultStream).Err(); err != nil {
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
			"-worker-token", cfg.workerToken,
			"-service-token", cfg.serviceToken,
			"-caller-node-id", childNodeID,
			"-submissions-root", filepath.Join(cfg.workRoot, "submissions"),
		},
		env: noProxyEnv(map[string]string{
			"OJOS_JUDGE_TASK_STREAM":   cfg.taskStream,
			"OJOS_JUDGE_RESULT_STREAM": cfg.resultStream,
		}),
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

	taskID, err := findTaskEntry(ctx, redisClient, cfg, submissionID)
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

	pending, err := pendingCount(ctx, redisClient, cfg)
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

	resultID, resultStatus, err := findResultEntry(ctx, redisClient, cfg, submissionID)
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

	if cfg.mode == "beta-local" && cfg.controlPlaneMode == "real" && cfg.installMode == "release-install" {
		if err := verifyMainServiceLocalProcessStarts(ctx, cfg); err != nil {
			return err
		}
	}

	fmt.Printf("[OK] smoke summary: submission_id=%d task_entry_id=%s result_entry_id=%s status=%s\n", submissionID, taskID, resultID, status)
	fmt.Printf("[OK] smoke logs: %s\n", filepath.Join(cfg.workRoot, "logs"))
	return nil
}

func runCompose(ctx context.Context, cfg smokeConfig) error {
	redisClient, err := connectRedis(ctx, cfg.redisURL)
	if err != nil {
		return fail("compose redis connected", err)
	}
	defer redisClient.Close()
	if cfg.cleanStreams {
		if err := redisClient.Del(ctx, cfg.taskStream, cfg.resultStream).Err(); err != nil {
			return fail("compose redis streams cleaned", err)
		}
	}
	ok("compose redis connected")

	if err := composeRestartService(ctx, cfg, "storage-service"); err != nil {
		return fail("compose storage-service backend "+cfg.storageBackend, err)
	}
	ok("compose storage-service backend configured: %s", cfg.storageBackend)

	healthChecks := []struct {
		name string
		url  string
	}{
		{name: "compose gateway health", url: cfg.gateway.baseURL() + "/health"},
		{name: "compose auth-service health", url: cfg.auth.baseURL() + "/health"},
		{name: "compose storage-service health", url: cfg.storage.baseURL() + "/health"},
		{name: "compose judge-api health", url: cfg.judgeAPI.baseURL() + "/health"},
		{name: "compose orchestrator health", url: cfg.orchestrator.baseURL() + "/health"},
	}
	for _, check := range healthChecks {
		if err := waitHealth(ctx, check.url); err != nil {
			return fail(check.name, err)
		}
		ok(check.name)
	}

	storageIP, err := composeServiceIP(ctx, cfg, "storage-service")
	if err != nil {
		return fail("compose storage-service container endpoint", err)
	}
	cfg.storageProvider = endpoint{host: storageIP, port: 8085}
	ok("compose storage provider endpoint: %s", storageProviderEndpoint(cfg).baseURL())

	if err := seedRealNodeTree(ctx, cfg); err != nil {
		return err
	}
	storageEndpointID, err := installStorageReleaseCompose(ctx, cfg)
	if err != nil {
		return err
	}
	if err := composeRestartService(ctx, cfg, "judge-worker"); err != nil {
		return fail("compose judge-worker container prepared", err)
	}
	ok("compose judge-worker container prepared")
	if err := installComposeCallerIdentities(ctx, cfg); err != nil {
		return err
	}
	if err := reloadGatewayFromComposeSmoke(ctx, cfg); err != nil {
		return err
	}
	if err := waitGatewayRoute(ctx, cfg, "storage.object.put", storageEndpointID); err != nil {
		return fail("compose gateway route reload completed", err)
	}
	ok("compose gateway route reload completed")

	if err := verifyStorageBackend(ctx, cfg); err != nil {
		return err
	}
	if err := verifyStorageLifecycleThroughResolver(ctx, cfg); err != nil {
		return err
	}

	userID, userJWT, err := ensureComposeSmokeUser(ctx, cfg)
	if err != nil {
		return fail("compose auth user login", err)
	}
	cfg.composeUserJWT = userJWT
	ok("compose auth user login: user_id=%d", userID)

	problemID, err := ensureComposeJudgeProblemFixture(ctx, cfg)
	if err != nil {
		return fail("compose problem-service testdata chain", err)
	}
	cfg.composeProblemID = problemID
	ok("compose problem-service testdata chain: problem_id=%d", problemID)

	if err := composeRestartService(ctx, cfg, "judge-worker"); err != nil {
		return fail("compose judge-worker restarted after stream reset", err)
	}
	ok("compose judge-worker restarted after stream reset")

	submissionID, err := createSubmissionViaGateway(ctx, cfg)
	if err != nil {
		return fail("compose submission created through gateway", err)
	}
	ok("compose submission created through gateway: %d", submissionID)

	sourceKey := fmt.Sprintf("%d-source-main.cpp", submissionID)
	if err := waitStorageObject(ctx, cfg, "submissions", sourceKey, submissionSourceCode); err != nil {
		return fail("compose source stored through internal resolver", err)
	}
	ok("compose source stored through internal resolver")

	taskID, err := findTaskEntry(ctx, redisClient, cfg, submissionID)
	if err != nil {
		return fail("compose redis task written", err)
	}
	cfg.lastTaskID = taskID
	ok("compose redis task written: %s", taskID)

	resultID, resultStatus, err := findResultEntry(ctx, redisClient, cfg, submissionID)
	if err != nil {
		return fail("compose result written", err)
	}
	cfg.lastResultID = resultID
	if resultStatus != "ACCEPTED" {
		return fail("compose result written", fmt.Errorf("unexpected result status %q", resultStatus))
	}
	ok("compose result written: %s", resultID)

	pending, err := pendingCount(ctx, redisClient, cfg)
	if err != nil {
		return fail("compose worker consumed task acked", err)
	}
	if pending != 0 {
		return fail("compose worker consumed task acked", fmt.Errorf("pending count is %d", pending))
	}
	ok("compose worker consumed task acked")

	status, err := waitSubmissionStatusViaGateway(ctx, cfg, submissionID)
	if err != nil {
		return fail("compose result query returned ACCEPTED", err)
	}
	if status != "ACCEPTED" {
		return fail("compose result query returned ACCEPTED", fmt.Errorf("unexpected status %q", status))
	}
	if err := querySubmissionCasesViaGateway(ctx, cfg, submissionID); err != nil {
		return fail("compose result query returned ACCEPTED", err)
	}
	ok("compose result query returned ACCEPTED")
	if err := verifyQueueStatusAPIViaGateway(ctx, cfg); err != nil {
		return err
	}

	fmt.Printf("[OK] compose smoke summary: submission_id=%d task_entry_id=%s result_entry_id=%s status=%s worker=compose runner=nsjail reload=smoke-driven\n", submissionID, taskID, resultID, status)
	return nil
}

func startWorker(ctx context.Context, cfg smokeConfig) (*childProcess, error) {
	env := noProxyEnv(map[string]string{
		"LANGUAGES_CONFIG":               filepath.Join(cfg.repoRoot, "services", "judge-worker", "config", "languages.yaml"),
		"OJOS_JUDGE_API_URL":             cfg.judgeAPI.baseURL(),
		"OJOS_WORKER_TOKEN":              cfg.workerToken,
		"OJOS_WORKER_ID":                 workerEndpointID,
		"OJOS_WORKER_NAME":               "smoke-judge-worker",
		"OJOS_MAX_CONCURRENCY":           "1",
		"OJOS_WORK_DIR":                  filepath.Join(cfg.workRoot, "worker-work"),
		"OJOS_ARTIFACT_CACHE_DIR":        filepath.Join(cfg.workRoot, "worker-cache"),
		"OJOS_SUPPORTED_LANGUAGES":       "cpp17",
		"OJOS_REDIS_URL":                 cfg.redisURL,
		"OJOS_JUDGE_TASK_STREAM":         cfg.taskStream,
		"OJOS_JUDGE_CONSUMER_GROUP":      consumerGroup,
		"OJOS_INTERNAL_GATEWAY_URL":      cfg.gateway.baseURL(),
		"OJOS_STORAGE_OBJECT_GET_API_ID": "storage.object.get",
		"OJOS_STORAGE_OBJECT_PUT_API_ID": "storage.object.put",
		"OJOS_SERVICE_TOKEN":             cfg.serviceToken,
		"OJOS_CALLER_NODE_ID":            childNodeID,
		"OJOS_RUNNER_MODE":               "nsjail",
		"OJOS_ALLOW_CGROUP_FALLBACK":     envDefault("OJOS_ALLOW_CGROUP_FALLBACK", "false"),
		"OJOS_NSJAIL_NO_PIVOTROOT":       envDefault("OJOS_NSJAIL_NO_PIVOTROOT", "false"),
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
			"AUTH_SERVICE_ADMIN_TOKEN":                 cfg.serviceToken,
			"ORCHESTRATOR_GATEWAY_ROUTE_PUBLISH":       "1",
			"GATEWAY_ENDPOINT":                         cfg.gateway.baseURL(),
			"GATEWAY_ADMIN_TOKEN":                      cfg.gatewayAdminJWT,
			"GATEWAY_NODE_ID":                          childNodeID,
			"OJOS_STORAGE_SERVICE_CONFIG":              storageConfigPath(cfg),
			"OJOS_AUTH_SERVICE_CONFIG":                 serviceStartAuthConfigPath(cfg),
			"OJOS_GATEWAY_CONFIG":                      serviceStartGatewayConfigPath(cfg),
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
			"AUTH_INTERNAL_TOKEN": cfg.serviceToken,
		}),
	})
}

func seedRealOrchestrator(ctx context.Context, cfg smokeConfig) error {
	provider := storageProviderEndpoint(cfg)
	endpointID := fmt.Sprintf("%s:%d:%s", provider.host, provider.port, storageService)
	body := map[string]any{
		"root_node_id":         rootNodeID,
		"root_host_ip":         provider.host,
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
	existing, err := existingNodeIDs(ctx, cfg)
	if err != nil {
		return fail("nodes created through API", err)
	}
	nodes := []map[string]any{
		{
			"node_id":        rootNodeID,
			"host_ip":        storageProviderEndpoint(cfg).host,
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
		nodeID, _ := body["node_id"].(string)
		if existing[nodeID] {
			continue
		}
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

func existingNodeIDs(ctx context.Context, cfg smokeConfig) (map[string]bool, error) {
	var tree struct {
		Nodes []struct {
			NodeID string `json:"node_id"`
		} `json:"nodes"`
	}
	status, body, err := doStatus(ctx, http.MethodGet, cfg.orchestrator.baseURL()+"/nodes", nil, map[string]string{})
	if err != nil {
		return nil, err
	}
	if status == http.StatusNotFound {
		return map[string]bool{}, nil
	}
	if status < 200 || status >= 300 {
		return nil, fmt.Errorf("GET /nodes returned %d: %s", status, strings.TrimSpace(string(body)))
	}
	if err := json.Unmarshal(body, &tree); err != nil {
		return nil, err
	}
	existing := map[string]bool{}
	for _, node := range tree.Nodes {
		if strings.TrimSpace(node.NodeID) != "" {
			existing[node.NodeID] = true
		}
	}
	return existing, nil
}

func verifyStorageRouteAbsentBeforeInstall(ctx context.Context, cfg smokeConfig) error {
	if err := verifyRouteMissing(ctx, cfg, "storage.object.get"); err != nil {
		return fail("release.install before route absent", err)
	}
	status, body, err := doStatus(ctx, http.MethodGet, cfg.gateway.baseURL()+"/internal/apis/storage.object.get/submissions/pre-install.txt", nil, map[string]string{
		"Authorization":         "Bearer " + cfg.serviceToken,
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
	provider := storageProviderEndpoint(cfg)
	endpointID := fmt.Sprintf("%s:%d:%s", provider.host, provider.port, storageService)
	body := map[string]any{
		"operation_id":             "op-smoke-storage-release-install",
		"host_ip":                  provider.host,
		"endpoint":                 endpointID,
		"gateway_node_id":          childNodeID,
		"execute_service_driver":   true,
		"external_service_running": false,
	}
	if err := addRequiredReleasePackageFields(body, cfg, storageService); err != nil {
		return "", fail("release.install storage-service", err)
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
	if err := verifyReleasePackageInstall(ctx, cfg, "op-smoke-storage-release-install", storageService); err != nil {
		return "", err
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
		if item.serviceName == judgeAPIService {
			if err := addRequiredReleasePackageFields(body, cfg, item.serviceName); err != nil {
				return fail("release.install service identities", err)
			}
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
		if item.serviceName == judgeAPIService {
			if err := verifyReleasePackageInstall(ctx, cfg, "op-smoke-"+item.serviceName+"-identity-install", item.serviceName); err != nil {
				return err
			}
		}
	}
	ok("release.install service identities: judge-api and judge-worker")
	return nil
}

func installStorageReleaseCompose(ctx context.Context, cfg smokeConfig) (string, error) {
	provider := storageProviderEndpoint(cfg)
	endpointID := fmt.Sprintf("%s:%d:%s", provider.host, provider.port, storageService)
	body := map[string]any{
		"operation_id":             fmt.Sprintf("op-compose-storage-release-install-%d", time.Now().UnixNano()),
		"host_ip":                  provider.host,
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
	if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.orchestrator.baseURL()+"/releases/"+storageService+"/install", body, map[string]string{}, &installResp); err != nil {
		return "", fail("compose release.install storage-service", err)
	}
	if installResp.ActionResult.ActionID != "release.install" {
		return "", fail("compose release.install storage-service", fmt.Errorf("unexpected action_id %q", installResp.ActionResult.ActionID))
	}
	if !strings.EqualFold(strings.ReplaceAll(installResp.ActionResult.Status, "_", ""), "succeeded") {
		return "", fail("compose release.install storage-service", fmt.Errorf("unexpected status %q operation_id=%s error=%s message=%s", installResp.ActionResult.Status, installResp.ActionResult.OperationID, installResp.ActionResult.Error, installResp.ActionResult.Message))
	}
	ok("compose release.install storage-service")
	if err := verifyRealOrchestratorRoutes(ctx, cfg, endpointID); err != nil {
		return "", err
	}
	return endpointID, nil
}

func installComposeCallerIdentities(ctx context.Context, cfg smokeConfig) error {
	installs := []struct {
		serviceName string
		port        int
	}{
		{serviceName: judgeAPIService, port: 8082},
		{serviceName: workerService, port: 9101},
		{serviceName: problemService, port: 8083},
	}
	for _, item := range installs {
		hostIP, err := composeServiceIP(ctx, cfg, item.serviceName)
		if err != nil {
			return fail("compose release.install service identities", err)
		}
		endpointID := fmt.Sprintf("%s:%d:%s", hostIP, item.port, item.serviceName)
		body := map[string]any{
			"operation_id":             fmt.Sprintf("op-compose-%s-identity-install-%d", item.serviceName, time.Now().UnixNano()),
			"host_ip":                  hostIP,
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
			return fail("compose release.install service identities", err)
		}
		if installResp.ActionResult.ActionID != "release.install" {
			return fail("compose release.install service identities", fmt.Errorf("%s unexpected action_id %q", item.serviceName, installResp.ActionResult.ActionID))
		}
		if !strings.EqualFold(strings.ReplaceAll(installResp.ActionResult.Status, "_", ""), "succeeded") {
			return fail("compose release.install service identities", fmt.Errorf("%s unexpected status %q operation_id=%s error=%s message=%s", item.serviceName, installResp.ActionResult.Status, installResp.ActionResult.OperationID, installResp.ActionResult.Error, installResp.ActionResult.Message))
		}
	}
	ok("compose release.install service identities: judge-api, judge-worker, and problem-service")
	return nil
}

func verifyMainServiceLocalProcessStarts(ctx context.Context, cfg smokeConfig) error {
	probes := []struct {
		serviceName string
		endpoint    endpoint
	}{
		{serviceName: "auth-service", endpoint: cfg.authServiceStart},
		{serviceName: "gateway", endpoint: cfg.gatewayStart},
	}
	for _, probe := range probes {
		operationID := fmt.Sprintf("op-smoke-%s-local-process-start", probe.serviceName)
		endpointID := fmt.Sprintf("%s:%d:%s", probe.endpoint.host, probe.endpoint.port, probe.serviceName)
		body := map[string]any{
			"operation_id":             operationID,
			"host_ip":                  probe.endpoint.host,
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
		if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.orchestrator.baseURL()+"/releases/"+probe.serviceName+"/install", body, map[string]string{}, &installResp); err != nil {
			return fail("release.install service_start "+probe.serviceName, err)
		}
		if installResp.ActionResult.ActionID != "release.install" {
			return fail("release.install service_start "+probe.serviceName, fmt.Errorf("unexpected action_id %q", installResp.ActionResult.ActionID))
		}
		if !strings.EqualFold(strings.ReplaceAll(installResp.ActionResult.Status, "_", ""), "succeeded") {
			return fail("release.install service_start "+probe.serviceName, fmt.Errorf("unexpected status %q operation_id=%s error=%s message=%s", installResp.ActionResult.Status, installResp.ActionResult.OperationID, installResp.ActionResult.Error, installResp.ActionResult.Message))
		}
		pid, err := releaseInstallDriverPID(ctx, cfg, operationID)
		if err != nil {
			return fail("release.install service_start "+probe.serviceName, err)
		}
		if err := waitHealth(ctx, probe.endpoint.baseURL()+"/health"); err != nil {
			return fail("release.install service_start "+probe.serviceName, err)
		}
		ok("release.install service_start %s local-process pid=%d", probe.serviceName, pid)
		if err := rollbackReleaseInstall(ctx, cfg, operationID, "release.install rollback stopped "+probe.serviceName); err != nil {
			return err
		}
		if err := waitEndpointUnavailable(ctx, probe.endpoint.baseURL()+"/health", probe.serviceName); err != nil {
			return fail("release.install rollback stopped "+probe.serviceName, err)
		}
		ok("release.install rollback stopped %s", probe.serviceName)
	}
	return nil
}

func releaseInstallDriverPID(ctx context.Context, cfg smokeConfig, operationID string) (uint64, error) {
	var resp struct {
		Logs []struct {
			StepID string         `json:"step_id"`
			Data   map[string]any `json:"data"`
		} `json:"logs"`
	}
	target := cfg.orchestrator.baseURL() + "/operations/" + operationID + "/logs"
	if err := doJSONWithHeaders(ctx, http.MethodGet, target, nil, map[string]string{}, &resp); err != nil {
		return 0, err
	}
	for _, log := range resp.Logs {
		if log.StepID != "driver:release.install" {
			continue
		}
		status, _ := log.Data["status"].(string)
		if !strings.EqualFold(status, "SUCCEEDED") {
			return 0, fmt.Errorf("driver status is %q", status)
		}
		switch raw := log.Data["pid"].(type) {
		case float64:
			if raw > 0 {
				return uint64(raw), nil
			}
		case json.Number:
			pid, err := raw.Int64()
			if err == nil && pid > 0 {
				return uint64(pid), nil
			}
		}
	}
	return 0, fmt.Errorf("driver pid log not found for %s", operationID)
}

func reloadGatewayFromComposeSmoke(ctx context.Context, cfg smokeConfig) error {
	var resp struct {
		Status     string `json:"status"`
		RouteCount int    `json:"route_count"`
	}
	headers := map[string]string{"Authorization": "Bearer " + cfg.gatewayAdminJWT}
	if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.gateway.baseURL()+"/api/admin/orchestrator/routes/reload", map[string]any{}, headers, &resp); err != nil {
		return fail("compose gateway reload smoke-driven", err)
	}
	if !strings.EqualFold(resp.Status, "reloaded") {
		return fail("compose gateway reload smoke-driven", fmt.Errorf("unexpected status %q", resp.Status))
	}
	ok("compose gateway reload smoke-driven: routes=%d", resp.RouteCount)
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
		if route.ProviderNodeID != rootNodeID || route.ProviderEndpoint != endpointID || route.UpstreamBase != storageProviderEndpoint(cfg).baseURL() {
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
	if err := rollbackReleaseInstall(ctx, cfg, "op-smoke-storage-release-install", "release.install rollback stopped storage-service"); err != nil {
		return err
	}
	if err := waitStorageUnavailable(ctx, cfg.storage.baseURL()+"/health"); err != nil {
		return fail("release.install rollback stopped storage-service", err)
	}
	ok("release.install rollback stopped storage-service")
	return nil
}

func rollbackReleaseInstall(ctx context.Context, cfg smokeConfig, operationID string, stepName string) error {
	var resp struct {
		ActionResult struct {
			ActionID    string `json:"action_id"`
			Status      string `json:"status"`
			OperationID string `json:"operation_id"`
			Error       string `json:"error"`
			Message     string `json:"message"`
		} `json:"action_result"`
	}
	target := cfg.orchestrator.baseURL() + "/operations/" + operationID + "/rollback"
	if err := doJSONWithHeaders(ctx, http.MethodPost, target, map[string]any{}, map[string]string{}, &resp); err != nil {
		return fail(stepName, err)
	}
	normalizedStatus := strings.ToLower(strings.ReplaceAll(resp.ActionResult.Status, "_", ""))
	if normalizedStatus != "succeeded" && normalizedStatus != "rolledback" {
		return fail(stepName, fmt.Errorf("unexpected rollback status %q operation_id=%s error=%s message=%s", resp.ActionResult.Status, resp.ActionResult.OperationID, resp.ActionResult.Error, resp.ActionResult.Message))
	}
	return nil
}

func waitGatewayRoute(ctx context.Context, cfg smokeConfig, apiID string, endpointID string) error {
	deadline := time.Now().Add(20 * time.Second)
	var last error
	for time.Now().Before(deadline) {
		status, body, err := doStatus(ctx, http.MethodPut, cfg.gateway.baseURL()+"/internal/apis/"+apiID+"/submissions/reload-probe.txt", map[string]string{
			"probe": "release-install-gateway-reload",
		}, map[string]string{
			"Authorization":         "Bearer " + cfg.serviceToken,
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
		"Authorization":         "Bearer " + cfg.serviceToken,
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
		"Authorization":         "Bearer " + cfg.serviceToken,
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
	allowed, status, err := permissionCheck(ctx, cfg.auth.baseURL(), cfg.serviceToken, judgeAPIService, "storage.object.put", "storage.object.write")
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

	allowed, status, err = permissionCheck(ctx, cfg.auth.baseURL(), cfg.serviceToken, judgeAPIService, "storage.object.delete", "storage.object.delete")
	if err != nil {
		return fail("permission-check denied missing permission", err)
	}
	if status != http.StatusOK || allowed {
		return fail("permission-check denied missing permission", fmt.Errorf("status=%d allowed=%v", status, allowed))
	}
	ok("permission-check denied missing permission")

	allowed, status, err = permissionCheck(ctx, cfg.auth.baseURL(), cfg.serviceToken, "unknown-worker", "storage.object.get", "storage.object.read")
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
	headers := map[string]string{"Authorization": "Bearer " + cfg.serviceToken}
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
		"Authorization":         "Bearer " + cfg.serviceToken,
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

	headers["X-OJOS-Caller-Service"] = "unknown-worker"
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
	return writeGatewayConfigForEndpoint(cfg, filepath.Join(cfg.workRoot, "config", "gateway.yaml"), cfg.gateway, "gateway-smoke")
}

func writeGatewayConfigForEndpoint(cfg smokeConfig, path string, target endpoint, name string) (string, error) {
	authEndpoint := cfg.orchestrator.baseURL()
	if cfg.authMode == "real" {
		authEndpoint = cfg.auth.baseURL()
	}
	content := fmt.Sprintf(`Name: %s
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
`, name, target.host, target.port,
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
	return writeAuthConfigForEndpoint(cfg, filepath.Join(cfg.workRoot, "config", "auth.yaml"), cfg.auth, "auth-service-smoke")
}

func writeAuthConfigForEndpoint(cfg smokeConfig, path string, target endpoint, name string) (string, error) {
	content := fmt.Sprintf(`Name: %s
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
`, name, target.host, target.port, yamlString(cfg.serviceToken))
	return path, os.WriteFile(path, []byte(content), 0o644)
}

func serviceStartAuthConfigPath(cfg smokeConfig) string {
	return filepath.Join(cfg.workRoot, "config", "auth-service-local-process.yaml")
}

func serviceStartGatewayConfigPath(cfg smokeConfig) string {
	return filepath.Join(cfg.workRoot, "config", "gateway-local-process.yaml")
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

func createSubmissionViaGateway(ctx context.Context, cfg smokeConfig) (int64, error) {
	problemID := cfg.composeProblemID
	if problemID <= 0 {
		problemID = 1001
	}
	body := map[string]any{
		"problem_id": problemID,
		"language":   "cpp17",
		"code":       submissionSourceCode,
	}
	deadline := time.Now().Add(30 * time.Second)
	var lastErr error
	for time.Now().Before(deadline) {
		var resp struct {
			SubmissionID int64  `json:"submission_id"`
			Status       string `json:"status"`
		}
		if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.gateway.baseURL()+"/api/judge/submissions", body, composeUserHeaders(cfg), &resp); err == nil {
			if resp.SubmissionID <= 0 {
				return 0, fmt.Errorf("invalid submission id: %d", resp.SubmissionID)
			}
			return resp.SubmissionID, nil
		} else {
			lastErr = err
		}
		if wait(ctx, 250*time.Millisecond) != nil {
			return 0, ctx.Err()
		}
	}
	return 0, fmt.Errorf("problem projection was not accepted by judge-api within 30s: %w", lastErr)
}

func waitSubmissionStatusViaGateway(ctx context.Context, cfg smokeConfig, submissionID int64) (string, error) {
	deadline := time.Now().Add(30 * time.Second)
	var last string
	for time.Now().Before(deadline) {
		var resp struct {
			Status string `json:"status"`
		}
		err := doJSONWithHeaders(ctx, http.MethodGet, fmt.Sprintf("%s/api/judge/submissions/%d", cfg.gateway.baseURL(), submissionID), nil, composeUserHeaders(cfg), &resp)
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

func querySubmissionCasesViaGateway(ctx context.Context, cfg smokeConfig, submissionID int64) error {
	var resp struct {
		Cases []struct {
			Status string `json:"status"`
		} `json:"cases"`
	}
	if err := doJSONWithHeaders(ctx, http.MethodGet, fmt.Sprintf("%s/api/judge/submissions/%d/cases", cfg.gateway.baseURL(), submissionID), nil, composeUserHeaders(cfg), &resp); err != nil {
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

func verifyQueueStatusAPIViaGateway(ctx context.Context, cfg smokeConfig) error {
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
	if err := doJSONWithHeaders(ctx, http.MethodGet, cfg.gateway.baseURL()+"/api/judge/admin/queue/status", nil, composeAdminHeaders(cfg), &resp); err != nil {
		return fail("compose queue status API returned pending/lag", err)
	}
	if resp.TaskStream != cfg.taskStream || resp.ResultStream != cfg.resultStream || resp.Group != consumerGroup {
		return fail("compose queue status API returned pending/lag", fmt.Errorf("unexpected stream identity: %#v", resp))
	}
	if resp.PendingCount != 0 {
		return fail("compose queue status API returned pending/lag", fmt.Errorf("expected zero pending after ack, got %d", resp.PendingCount))
	}
	if resp.ConsumerCount == 0 || len(resp.Consumers) == 0 {
		return fail("compose queue status API returned pending/lag", fmt.Errorf("expected at least one consumer: %#v", resp))
	}
	lag := resp.Lag
	if lag < 0 {
		lag = resp.ConsumerLag
	}
	ok("compose queue status API returned pending=%d lag=%d consumers=%d", resp.PendingCount, lag, len(resp.Consumers))
	return nil
}

func ensureComposeJudgeProblemFixture(ctx context.Context, cfg smokeConfig) (int64, error) {
	if err := composeCommand(ctx, cfg, 90*time.Second, "run", "--rm", "judge-api-migrations"); err != nil {
		return 0, err
	}

	slug := fmt.Sprintf("compose-smoke-%d", time.Now().UnixNano())
	createBody := map[string]any{
		"title":           "Compose Smoke",
		"slug":            slug,
		"statement":       "Add two tokens and print ok for the smoke case.",
		"visibility":      "public",
		"time_limit_ms":   1000,
		"memory_limit_mb": 256,
	}
	var createResp struct {
		ProblemID int64  `json:"problem_id"`
		Slug      string `json:"slug"`
	}
	if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.gateway.baseURL()+"/api/problem/problems", createBody, composeUserHeaders(cfg), &createResp); err != nil {
		return 0, err
	}
	if createResp.ProblemID <= 0 || strings.TrimSpace(createResp.Slug) == "" {
		return 0, fmt.Errorf("invalid create problem response: %#v", createResp)
	}
	ok("compose problem-service created problem: problem_id=%d slug=%s", createResp.ProblemID, createResp.Slug)

	testBody := map[string]any{
		"case_no": 1,
		"input":   "1 1\n",
		"answer":  "ok\n",
		"score":   100,
		"sample":  true,
	}
	var testResp struct {
		CaseNo int `json:"case_no"`
	}
	if err := doJSONWithHeaders(ctx, http.MethodPost, fmt.Sprintf("%s/api/problem/problems/%d/test-cases", cfg.gateway.baseURL(), createResp.ProblemID), testBody, composeUserHeaders(cfg), &testResp); err != nil {
		return 0, err
	}
	if testResp.CaseNo != 1 {
		return 0, fmt.Errorf("unexpected test case response: %#v", testResp)
	}
	ok("compose problem-service added test case: problem_id=%d case_no=%d", createResp.ProblemID, testResp.CaseNo)

	if err := waitStorageObject(ctx, cfg, "problems", problemStorageObjectKey(createResp.ProblemID, "tests/001.in"), "1 1"); err != nil {
		return 0, err
	}
	if err := waitStorageObject(ctx, cfg, "problems", problemStorageObjectKey(createResp.ProblemID, "tests/001.ans"), "ok"); err != nil {
		return 0, err
	}
	ok("compose problem testdata stored through storage-service")

	ok("compose problem snapshot queued for automatic judge projection: problem_id=%d", createResp.ProblemID)
	return createResp.ProblemID, nil
}

func problemStorageObjectKey(problemID int64, logicalPath string) string {
	logicalPath = strings.Trim(strings.ReplaceAll(strings.TrimSpace(logicalPath), "\\", "/"), "/")
	if logicalPath == "" {
		logicalPath = "file"
	}
	replacer := strings.NewReplacer("/", "__", " ", "_", ":", "_")
	return fmt.Sprintf("problem-%d-%s", problemID, replacer.Replace(logicalPath))
}

func ensureComposeSmokeUser(ctx context.Context, cfg smokeConfig) (int64, string, error) {
	username := "compose-smoke"
	password := "compose-smoke-password"
	registerBody := map[string]any{
		"username": username,
		"email":    "compose-smoke@example.test",
		"password": password,
	}
	var registerResp struct {
		Code int    `json:"code"`
		Msg  string `json:"msg"`
		Data struct {
			UserID int64 `json:"user_id"`
		} `json:"data"`
	}
	if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.auth.baseURL()+"/auth/register", registerBody, nil, &registerResp); err != nil {
		return 0, "", err
	}
	if registerResp.Code != 0 && !strings.Contains(strings.ToLower(registerResp.Msg), "already exists") {
		return 0, "", fmt.Errorf("register compose user failed: code=%d msg=%s", registerResp.Code, registerResp.Msg)
	}

	loginBody := map[string]any{
		"username": username,
		"password": password,
	}
	var loginResp struct {
		Code int    `json:"code"`
		Msg  string `json:"msg"`
		Data struct {
			Token  string `json:"token"`
			UserID int64  `json:"user_id"`
		} `json:"data"`
	}
	if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.auth.baseURL()+"/auth/login", loginBody, nil, &loginResp); err != nil {
		return 0, "", err
	}
	if loginResp.Code != 0 {
		return 0, "", fmt.Errorf("login compose user failed: code=%d msg=%s", loginResp.Code, loginResp.Msg)
	}
	if strings.TrimSpace(loginResp.Data.Token) == "" || loginResp.Data.UserID <= 0 {
		return 0, "", fmt.Errorf("invalid compose user login response: user_id=%d token_empty=%t", loginResp.Data.UserID, strings.TrimSpace(loginResp.Data.Token) == "")
	}
	if err := grantComposeUserRole(ctx, cfg, loginResp.Data.UserID, "problem_setter"); err != nil {
		return 0, "", err
	}
	if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.auth.baseURL()+"/auth/login", loginBody, nil, &loginResp); err != nil {
		return 0, "", err
	}
	if loginResp.Code != 0 {
		return 0, "", fmt.Errorf("login compose user after role grant failed: code=%d msg=%s", loginResp.Code, loginResp.Msg)
	}
	return loginResp.Data.UserID, loginResp.Data.Token, nil
}

func grantComposeUserRole(ctx context.Context, cfg smokeConfig, userID int64, role string) error {
	body := map[string]any{
		"user_id": userID,
		"role":    role,
	}
	var resp struct {
		Code int    `json:"code"`
		Msg  string `json:"msg"`
	}
	if err := doJSONWithHeaders(ctx, http.MethodPost, cfg.auth.baseURL()+"/auth/admin/users/roles", body, composeAdminHeaders(cfg), &resp); err != nil {
		return err
	}
	if resp.Code != 0 {
		return fmt.Errorf("grant compose user role failed: code=%d msg=%s", resp.Code, resp.Msg)
	}
	ok("compose auth user role granted: user_id=%d role=%s", userID, role)
	return nil
}

func composeCommand(parent context.Context, cfg smokeConfig, timeout time.Duration, args ...string) error {
	composePath := filepath.Join(cfg.repoRoot, "deploy", "compose", "docker-compose.yml")
	ctx, cancel := matrixContext(parent, timeout)
	defer cancel()
	commandArgs := append([]string{"compose", "-f", composePath}, args...)
	cmd := exec.CommandContext(ctx, "docker", commandArgs...)
	cmd.Dir = cfg.repoRoot
	cmd.Env = composeProcessEnv(cfg)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("docker %s failed: %s", strings.Join(commandArgs, " "), oneLine(out, err))
	}
	return nil
}

func composeUserHeaders(cfg smokeConfig) map[string]string {
	token := strings.TrimSpace(cfg.composeUserJWT)
	if token == "" {
		token, _ = sharedjwt.Generate(envDefault("JWT_SECRET", "ojos-local-jwt"), 7, "compose-smoke", []string{"user"}, 1)
	}
	return map[string]string{
		"Authorization":  "Bearer " + token,
		"X-OJOS-Node-Id": childNodeID,
	}
}

func composeAdminHeaders(cfg smokeConfig) map[string]string {
	return map[string]string{
		"Authorization":  "Bearer " + cfg.gatewayAdminJWT,
		"X-OJOS-Node-Id": childNodeID,
	}
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

func findTaskEntry(ctx context.Context, client *redis.Client, cfg smokeConfig, submissionID int64) (string, error) {
	wantSubmission := strconv.FormatInt(submissionID, 10)
	wantTask := "sub-" + wantSubmission
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		entries, err := client.XRange(ctx, cfg.taskStream, "-", "+").Result()
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

func findResultEntry(ctx context.Context, client *redis.Client, cfg smokeConfig, submissionID int64) (string, string, error) {
	wantSubmission := strconv.FormatInt(submissionID, 10)
	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		entries, err := client.XRange(ctx, cfg.resultStream, "-", "+").Result()
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

func pendingCount(ctx context.Context, client *redis.Client, cfg smokeConfig) (int64, error) {
	value, err := client.Do(ctx, "XPENDING", cfg.taskStream, consumerGroup).Result()
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
	if resp.TaskStream != cfg.taskStream || resp.ResultStream != cfg.resultStream || resp.Group != consumerGroup {
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
	return waitEndpointUnavailable(ctx, target, "storage-service")
}

func waitEndpointUnavailable(ctx context.Context, target string, serviceName string) error {
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
	return fmt.Errorf("%s still responds after rollback", serviceName)
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

func prepareReleasePackages(cfg smokeConfig) (map[string]releasePackageInfo, error) {
	packagesDir := filepath.Join(cfg.workRoot, "packages")
	if err := os.MkdirAll(packagesDir, 0o755); err != nil {
		return nil, err
	}
	specs := []struct {
		serviceName string
		outputPath  string
	}{
		{serviceName: storageService, outputPath: cfg.storagePackage},
		{serviceName: judgeAPIService, outputPath: cfg.judgeAPIPackage},
	}
	packages := make(map[string]releasePackageInfo, len(specs))
	for _, spec := range specs {
		outputPath := strings.TrimSpace(spec.outputPath)
		if outputPath == "" {
			outputPath = filepath.Join(packagesDir, spec.serviceName+"-release.zip")
		}
		info, err := buildReleasePackage(cfg.repoRoot, spec.serviceName, outputPath)
		if err != nil {
			return nil, err
		}
		packages[spec.serviceName] = info
		ok("release package prepared: %s checksum=%s", spec.serviceName, info.checksum)
	}
	return packages, nil
}

func buildReleasePackage(repoRoot string, serviceName string, outputPath string) (releasePackageInfo, error) {
	serviceDir := filepath.Join(repoRoot, "services", serviceName)
	releasePath := filepath.Join(serviceDir, "release.yaml")
	if _, err := os.Stat(releasePath); err != nil {
		return releasePackageInfo{}, err
	}
	absOutput, err := filepath.Abs(outputPath)
	if err != nil {
		return releasePackageInfo{}, err
	}
	if err := os.MkdirAll(filepath.Dir(absOutput), 0o755); err != nil {
		return releasePackageInfo{}, err
	}
	relOutput, err := filepath.Rel(repoRoot, absOutput)
	if err != nil {
		return releasePackageInfo{}, err
	}
	if strings.HasPrefix(relOutput, ".."+string(filepath.Separator)) || relOutput == ".." || filepath.IsAbs(relOutput) {
		return releasePackageInfo{}, fmt.Errorf("release package output must be under repo root for local loader: %s", absOutput)
	}
	sourceURL := filepath.ToSlash(relOutput)
	releaseYAML, err := os.ReadFile(releasePath)
	if err != nil {
		return releasePackageInfo{}, err
	}
	rewrittenReleaseYAML := rewriteReleaseSource(string(releaseYAML), sourceURL)

	out, err := os.Create(absOutput)
	if err != nil {
		return releasePackageInfo{}, err
	}
	zipWriter := zip.NewWriter(out)
	closeErr := func() error {
		if err := zipWriter.Close(); err != nil {
			_ = out.Close()
			return err
		}
		return out.Close()
	}
	if err := addZipBytes(zipWriter, filepath.ToSlash(filepath.Join(serviceName, "release.yaml")), []byte(rewrittenReleaseYAML)); err != nil {
		_ = out.Close()
		return releasePackageInfo{}, err
	}
	err = filepath.WalkDir(serviceDir, func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if path == serviceDir {
			return nil
		}
		rel, err := filepath.Rel(serviceDir, path)
		if err != nil {
			return err
		}
		if shouldSkipReleasePackageEntry(rel, entry) {
			if entry.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}
		if entry.IsDir() {
			return nil
		}
		if filepath.ToSlash(rel) == "release.yaml" {
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if info.Mode()&os.ModeSymlink != 0 {
			return nil
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		entryName := filepath.ToSlash(filepath.Join(serviceName, rel))
		return addZipBytes(zipWriter, entryName, data)
	})
	if err != nil {
		_ = out.Close()
		return releasePackageInfo{}, err
	}
	if err := closeErr(); err != nil {
		return releasePackageInfo{}, err
	}
	body, err := os.ReadFile(absOutput)
	if err != nil {
		return releasePackageInfo{}, err
	}
	sum := sha256.Sum256(body)
	return releasePackageInfo{
		path:      absOutput,
		sourceURL: sourceURL,
		checksum:  fmt.Sprintf("sha256:%x", sum),
	}, nil
}

func rewriteReleaseSource(text string, sourceURL string) string {
	lines := strings.Split(text, "\n")
	inSource := false
	for i, line := range lines {
		trimmed := strings.TrimSpace(line)
		if trimmed == "source:" {
			inSource = true
			continue
		}
		if inSource && trimmed != "" && !strings.HasPrefix(line, " ") && !strings.HasPrefix(line, "\t") {
			inSource = false
		}
		if !inSource {
			continue
		}
		switch {
		case strings.HasPrefix(trimmed, "url:"):
			lines[i] = "  url: " + yamlString(sourceURL)
		case strings.HasPrefix(trimmed, "checksum:"):
			lines[i] = `  checksum: ""`
		}
	}
	return strings.Join(lines, "\n")
}

func addZipBytes(writer *zip.Writer, name string, data []byte) error {
	header := &zip.FileHeader{
		Name:   filepath.ToSlash(name),
		Method: zip.Deflate,
	}
	header.SetMode(0o644)
	fileWriter, err := writer.CreateHeader(header)
	if err != nil {
		return err
	}
	_, err = fileWriter.Write(data)
	return err
}

func shouldSkipReleasePackageEntry(rel string, entry fs.DirEntry) bool {
	name := entry.Name()
	if name == "" {
		return true
	}
	switch name {
	case ".git", ".smoke", "node_modules", "target", "dist", "build", ".next", ".turbo", ".cache", "tmp", "coverage":
		return true
	}
	rel = filepath.ToSlash(rel)
	return strings.HasPrefix(rel, ".smoke/")
}

func addRequiredReleasePackageFields(body map[string]any, cfg smokeConfig, serviceName string) error {
	if cfg.releaseSource != "package" {
		return nil
	}
	info, found := cfg.releasePackages[serviceName]
	if !found {
		return fmt.Errorf("release package for %s is not prepared", serviceName)
	}
	body["release_url"] = info.sourceURL
	body["release_checksum"] = info.checksum
	return nil
}

func verifyReleasePackageInstall(ctx context.Context, cfg smokeConfig, operationID string, serviceName string) error {
	if cfg.releaseSource != "package" {
		return nil
	}
	info, found := cfg.releasePackages[serviceName]
	if !found {
		return fail("release package install", fmt.Errorf("release package for %s is not prepared", serviceName))
	}
	var resp struct {
		Logs []struct {
			StepID string         `json:"step_id"`
			Data   map[string]any `json:"data"`
		} `json:"logs"`
	}
	target := cfg.orchestrator.baseURL() + "/operations/" + operationID + "/logs"
	if err := doJSONWithHeaders(ctx, http.MethodGet, target, nil, map[string]string{}, &resp); err != nil {
		return fail("release package install", err)
	}
	for _, log := range resp.Logs {
		if log.StepID != "release-package:"+serviceName {
			continue
		}
		status, _ := log.Data["status"].(string)
		sourceURL, _ := log.Data["source_url"].(string)
		checksum, _ := log.Data["checksum"].(string)
		manifestLoaded, _ := log.Data["manifest_loaded"].(bool)
		if status == "loaded" && sourceURL == info.sourceURL && checksum == info.checksum && manifestLoaded {
			ok("release package loaded: %s", serviceName)
			ok("release package checksum verified: %s", serviceName)
			ok("release.install from package: %s path=%s", serviceName, info.path)
			return nil
		}
	}
	return fail("release package install", fmt.Errorf("%s package load log not found or mismatched: %#v", serviceName, resp.Logs))
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
	case "compose":
		return "compose"
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

func normalizeReleaseSource(value string) string {
	value = strings.ToLower(strings.TrimSpace(value))
	switch value {
	case "":
		return ""
	case "tree", "source-tree", "source":
		return "tree"
	case "package", "archive":
		return "package"
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
	if cfg.releaseSource == "package" {
		ok("release package install in smoke")
	} else {
		skip("release package install in smoke: source tree release.yaml")
	}
	ok("release.install storage-service")
	ok("release.install service_start storage-service local-process")
	ok("release.install service_start auth-service local-process")
	ok("release.install service_start gateway local-process")
	ok("gateway reload orchestrator-driven")
	ok("storage backend %s", cfg.storageBackend)
	ok("Redis task/result")
	ok("queue pending/lag")
	ok("judge-worker nsjail runner")
	ok("result ACCEPTED")
	printComposeMatrixStatus(parent, cfg)
	if cfg.storageBackend == "minio" {
		ok("MinIO beta smoke: storage-service backend=minio")
	} else {
		skip("MinIO beta smoke: storage backend is local")
	}
	printMinIOMatrixStatus(parent, cfg)
	printNsjailMatrixStatus(parent, cfg)
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

func composeServiceIP(parent context.Context, cfg smokeConfig, service string) (string, error) {
	composePath := filepath.Join(cfg.repoRoot, "deploy", "compose", "docker-compose.yml")
	ctx, cancel := matrixContext(parent, 20*time.Second)
	defer cancel()
	ps := exec.CommandContext(ctx, "docker", "compose", "-f", composePath, "ps", "-q", service)
	ps.Dir = cfg.repoRoot
	out, err := ps.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("docker compose ps -q %s failed: %s", service, oneLine(out, err))
	}
	containerID := strings.TrimSpace(string(out))
	if containerID == "" {
		return "", fmt.Errorf("compose service %s has no container id", service)
	}
	first := strings.Fields(containerID)[0]
	inspect := exec.CommandContext(ctx, "docker", "inspect", "-f", "{{range.NetworkSettings.Networks}}{{.IPAddress}}{{end}}", first)
	inspect.Dir = cfg.repoRoot
	out, err = inspect.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("docker inspect %s failed: %s", service, oneLine(out, err))
	}
	ip := strings.TrimSpace(string(out))
	if net.ParseIP(ip) == nil {
		return "", fmt.Errorf("compose service %s returned invalid ip %q", service, ip)
	}
	return ip, nil
}

func composeRestartService(parent context.Context, cfg smokeConfig, service string) error {
	composePath := filepath.Join(cfg.repoRoot, "deploy", "compose", "docker-compose.yml")
	ctx, cancel := matrixContext(parent, 240*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, "docker", "compose", "-f", composePath, "up", "-d", "--force-recreate", service)
	cmd.Dir = cfg.repoRoot
	cmd.Env = composeProcessEnv(cfg)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("docker compose restart %s failed: %s", service, oneLine(out, err))
	}
	deadline := time.Now().Add(60 * time.Second)
	for time.Now().Before(deadline) {
		statuses, err := composeServiceStatuses(parent, cfg)
		if err == nil {
			if status, ok := statuses[service]; ok && strings.EqualFold(strings.TrimSpace(status.State), "running") {
				return nil
			}
		}
		if wait(parent, 500*time.Millisecond) != nil {
			return parent.Err()
		}
	}
	return fmt.Errorf("compose service %s did not reach running state", service)
}

func composeProcessEnv(cfg smokeConfig) []string {
	env := noProxyEnv(map[string]string{
		"STORAGE_BACKEND":            cfg.storageBackend,
		"MINIO_ROOT_USER":            envDefault("MINIO_ROOT_USER", "ojos-minio"),
		"MINIO_ROOT_PASSWORD":        envDefault("MINIO_ROOT_PASSWORD", "ojos-minio-local"),
		"MINIO_ENDPOINT":             envDefault("MINIO_ENDPOINT", "minio:9000"),
		"MINIO_USE_SSL":              envDefault("MINIO_USE_SSL", "false"),
		"OJOS_STORAGE_BACKEND":       cfg.storageBackend,
		"OJOS_RUNNER_MODE":           "nsjail",
		"OJOS_ALLOW_CGROUP_FALLBACK": envDefault("OJOS_ALLOW_CGROUP_FALLBACK", "false"),
		"OJOS_NSJAIL_NO_PIVOTROOT":   envDefault("OJOS_NSJAIL_NO_PIVOTROOT", "false"),
	})
	return processEnv(env)
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

func printNsjailMatrixStatus(parent context.Context, cfg smokeConfig) {
	if runtime.GOOS != "linux" {
		skip("nsjail live: current OS is %s", runtime.GOOS)
		return
	}
	if _, err := exec.LookPath("nsjail"); err != nil {
		skip("nsjail live: nsjail unavailable on PATH")
		return
	}
	ctx, cancel := matrixContext(parent, 120*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, "cargo", "test", "nsjail_", "--", "--test-threads=1")
	cmd.Dir = filepath.Join(cfg.repoRoot, "services", "judge-worker")
	cmd.Env = processEnv(noProxyEnv(map[string]string{
		"OJOS_REQUIRE_NSJAIL_LIVE":   "1",
		"OJOS_ALLOW_CGROUP_FALLBACK": "false",
		"OJOS_NSJAIL_NO_PIVOTROOT":   envDefault("OJOS_NSJAIL_NO_PIVOTROOT", "false"),
		"CARGO_TARGET_DIR":           filepath.Join(cfg.workRoot, "target", "judge-worker-nsjail-live"),
	}))
	if out, err := cmd.CombinedOutput(); err != nil {
		skip("nsjail live: strict sandbox tests did not pass: %s", oneLine(out, err))
		return
	}
	ok("nsjail live: strict sandbox tests passed")
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
