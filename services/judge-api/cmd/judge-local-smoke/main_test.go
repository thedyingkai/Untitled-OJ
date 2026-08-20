package main

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
)

func TestWaitForNewRedisConsumerIgnoresBaselineAndReturnsNewName(t *testing.T) {
	redisServer := miniredis.RunT(t)
	client := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	t.Cleanup(func() { _ = client.Close() })
	ctx := context.Background()
	const stream = "test:judge:task"
	registerTestRedisConsumer(t, client, stream, "old-worker")
	baseline, err := loadRedisConsumerNames(ctx, client, stream, consumerGroup)
	if err != nil {
		t.Fatalf("load baseline: %v", err)
	}
	registerTestRedisConsumer(t, client, stream, "new-worker")

	name, err := waitForNewRedisConsumerWithTiming(
		ctx,
		client,
		stream,
		consumerGroup,
		baseline,
		100*time.Millisecond,
		time.Millisecond,
	)
	if err != nil {
		t.Fatalf("wait for new consumer: %v", err)
	}
	if name != "new-worker" {
		t.Fatalf("new consumer = %q, want new-worker", name)
	}
}

func TestWaitForNewRedisConsumerTimesOutWhenOnlyBaselineRemains(t *testing.T) {
	redisServer := miniredis.RunT(t)
	client := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	t.Cleanup(func() { _ = client.Close() })
	ctx := context.Background()
	const stream = "test:judge:task"
	registerTestRedisConsumer(t, client, stream, "old-worker")
	baseline, err := loadRedisConsumerNames(ctx, client, stream, consumerGroup)
	if err != nil {
		t.Fatalf("load baseline: %v", err)
	}

	_, err = waitForNewRedisConsumerWithTiming(
		ctx,
		client,
		stream,
		consumerGroup,
		baseline,
		20*time.Millisecond,
		time.Millisecond,
	)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("wait error = %v, want deadline exceeded", err)
	}
}

func TestRestartComposeWorkerCapturesBaselineBeforeRestartAndCreatesAfterNewConsumer(t *testing.T) {
	redisServer := miniredis.RunT(t)
	client := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	t.Cleanup(func() { _ = client.Close() })
	const stream = "test:judge:task"
	registerTestRedisConsumer(t, client, stream, "old-worker")
	var calls []string

	submissionID, err := restartComposeWorkerAndCreateSubmission(
		context.Background(),
		smokeConfig{taskStream: stream},
		client,
		func(_ context.Context, _ smokeConfig, service string) error {
			calls = append(calls, "restart:"+service)
			registerTestRedisConsumer(t, client, stream, "new-worker")
			return nil
		},
		func(_ context.Context, _ smokeConfig) (int64, error) {
			calls = append(calls, "create")
			return 42, nil
		},
	)
	if err != nil {
		t.Fatalf("restart worker and create submission: %v", err)
	}
	if submissionID != 42 {
		t.Fatalf("submission ID = %d, want 42", submissionID)
	}
	if want := []string{"restart:judge-worker", "create"}; !reflect.DeepEqual(calls, want) {
		t.Fatalf("calls = %#v, want %#v", calls, want)
	}
}

func registerTestRedisConsumer(t *testing.T, client *redis.Client, stream, consumer string) {
	t.Helper()
	ctx := context.Background()
	if err := client.XGroupCreateMkStream(ctx, stream, consumerGroup, "0").Err(); err != nil && !strings.Contains(err.Error(), "BUSYGROUP") {
		t.Fatalf("create consumer group: %v", err)
	}
	if err := client.XAdd(ctx, &redis.XAddArgs{Stream: stream, Values: map[string]any{"consumer": consumer}}).Err(); err != nil {
		t.Fatalf("add consumer event: %v", err)
	}
	if _, err := client.XReadGroup(ctx, &redis.XReadGroupArgs{
		Group: consumerGroup, Consumer: consumer, Streams: []string{stream, ">"}, Count: 1,
	}).Result(); err != nil {
		t.Fatalf("register consumer %s: %v", consumer, err)
	}
}

func TestValidateComposeQueueStatusRequiresConsumedRedisBoundary(t *testing.T) {
	cfg := smokeConfig{taskStream: taskStream, resultStream: resultStream}
	valid := composeQueueStatus{
		TaskStream: taskStream, ResultStream: resultStream, Group: consumerGroup,
		RedisStatus: "ok", ConsumerCount: 1, Lag: 0,
	}
	valid.Consumers = append(valid.Consumers, struct {
		Name       string `json:"name"`
		Pending    int64  `json:"pending"`
		IdleMs     int64  `json:"idle_ms"`
		InactiveMs int64  `json:"inactive_ms"`
	}{Name: "worker-a"})
	if err := validateComposeQueueStatus(valid, cfg); err != nil {
		t.Fatalf("valid queue status rejected: %v", err)
	}

	for name, mutate := range map[string]func(*composeQueueStatus){
		"lag":      func(status *composeQueueStatus) { status.Lag = 1 },
		"pending":  func(status *composeQueueStatus) { status.PendingCount = 1 },
		"consumer": func(status *composeQueueStatus) { status.ConsumerCount = 0 },
		"redis":    func(status *composeQueueStatus) { status.RedisStatus = "unavailable" },
	} {
		t.Run(name, func(t *testing.T) {
			status := valid
			mutate(&status)
			if err := validateComposeQueueStatus(status, cfg); err == nil {
				t.Fatalf("invalid %s status accepted: %#v", name, status)
			}
		})
	}
}

func TestEnsureComposeSmokeAdminUsesPersistedAdminIdentity(t *testing.T) {
	const (
		serviceToken = "compose-auth-management-token"
		adminToken   = "persisted-compose-admin-jwt"
	)

	var (
		assignmentAuthorization string
		assignmentUserID        int64
		assignmentPermission    string
		assignmentScopeType     string
		assignmentEffect        string
		loginCount              int
	)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch {
		case r.Method == http.MethodPost && r.URL.Path == "/auth/register":
			_, _ = w.Write([]byte(`{"code":0,"msg":"success","data":{"user_id":42}}`))
		case r.Method == http.MethodPost && r.URL.Path == "/auth/login":
			loginCount++
			roles := []string{"user"}
			permissions := []string{"judge.submit"}
			if loginCount > 1 {
				permissions = append(permissions, "judge.admin")
			}
			_ = json.NewEncoder(w).Encode(map[string]any{
				"code": 0,
				"msg":  "success",
				"data": map[string]any{
					"token":       adminToken,
					"user_id":     42,
					"roles":       roles,
					"permissions": permissions,
				},
			})
		case r.Method == http.MethodPost && r.URL.Path == "/auth/admin/permission-assignments":
			assignmentAuthorization = r.Header.Get("Authorization")
			var body struct {
				TargetType string `json:"target_type"`
				TargetID   int64  `json:"target_id"`
				Permission string `json:"permission"`
				ScopeType  string `json:"scope_type"`
				Effect     string `json:"effect"`
			}
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				t.Errorf("decode permission assignment: %v", err)
			}
			if body.TargetType != "user" {
				t.Errorf("assignment target_type = %q", body.TargetType)
			}
			assignmentUserID = body.TargetID
			assignmentPermission = body.Permission
			assignmentScopeType = body.ScopeType
			assignmentEffect = body.Effect
			_, _ = w.Write([]byte(`{"code":0,"msg":"success"}`))
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	userID, token, err := ensureComposeSmokeAdmin(context.Background(), smokeConfig{
		auth:         endpointFromServerURL(t, server.URL),
		serviceToken: serviceToken,
	})
	if err != nil {
		t.Fatalf("ensure compose admin: %v", err)
	}
	if userID != 42 || token != adminToken || loginCount != 2 {
		t.Fatalf("admin identity = user_id=%d token=%q login_count=%d", userID, token, loginCount)
	}
	if assignmentAuthorization != "Bearer "+serviceToken || assignmentUserID != 42 ||
		assignmentPermission != "judge.admin" || assignmentScopeType != "system" || assignmentEffect != "allow" {
		t.Fatalf(
			"judge admin assignment = auth=%q user_id=%d permission=%q scope=%q effect=%q",
			assignmentAuthorization,
			assignmentUserID,
			assignmentPermission,
			assignmentScopeType,
			assignmentEffect,
		)
	}

	cfg := smokeConfig{gatewayAdminJWT: "unrelated-gateway-management-jwt", composeAdminJWT: token}
	if got := composeJudgeAdminHeaders(cfg)["Authorization"]; got != "Bearer "+adminToken {
		t.Fatalf("judge admin authorization = %q", got)
	}
}

func TestEnsureComposeSmokeAdminRejectsMissingJudgePermission(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/auth/register", "/auth/admin/permission-assignments":
			_, _ = w.Write([]byte(`{"code":0,"msg":"success","data":{"user_id":42}}`))
		case "/auth/login":
			_, _ = w.Write([]byte(`{"code":0,"msg":"success","data":{"token":"admin-token","user_id":42,"roles":["user"],"permissions":["judge.submit"]}}`))
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	_, _, err := ensureComposeSmokeAdmin(context.Background(), smokeConfig{
		auth:         endpointFromServerURL(t, server.URL),
		serviceToken: "compose-auth-management-token",
	})
	if err == nil {
		t.Fatal("compose admin without judge.admin was accepted")
	}
}

func endpointFromServerURL(t *testing.T, raw string) endpoint {
	t.Helper()
	host, port, err := net.SplitHostPort(strings.TrimPrefix(raw, "http://"))
	if err != nil {
		t.Fatalf("parse test server URL: %v", err)
	}
	portNumber, err := strconv.Atoi(port)
	if err != nil {
		t.Fatalf("parse test server port: %v", err)
	}
	return endpoint{host: host, port: portNumber}
}

func TestComposeDefaultNetworkIPSelectsDefaultFromMultiNetworkContainer(t *testing.T) {
	got, err := composeDefaultNetworkIP([]byte(`{
		"ojos_platform-control":{"IPAddress":"172.21.0.4"},
		"ojos_default":{"IPAddress":"172.20.0.9"}
	}`))
	if err != nil {
		t.Fatalf("select default network: %v", err)
	}
	if got != "172.20.0.9" {
		t.Fatalf("default network ip = %q", got)
	}
}

func TestComposeDefaultNetworkIPRejectsAmbiguousNetworks(t *testing.T) {
	_, err := composeDefaultNetworkIP([]byte(`{
		"network-a":{"IPAddress":"172.21.0.4"},
		"network-b":{"IPAddress":"172.20.0.9"}
	}`))
	if err == nil {
		t.Fatal("ambiguous network addresses were accepted")
	}
}

func TestValidateComposeProblemTestCaseRequiresPersistedContent(t *testing.T) {
	want := composeProblemTestCase{
		No:     1,
		Input:  "001.in",
		Answer: "001.ans",
		Score:  100,
		Sample: true,
	}
	irrelevant := want
	irrelevant.No = 2
	if err := validateComposeProblemTestCase([]composeProblemTestCase{irrelevant, want}, want); err != nil {
		t.Fatalf("validate persisted testcase: %v", err)
	}

	wrong := want
	wrong.Answer = "wrong.ans"
	if err := validateComposeProblemTestCase([]composeProblemTestCase{wrong}, want); err == nil {
		t.Fatal("mismatched persisted testcase was accepted")
	}
	if err := validateComposeProblemTestCase(nil, want); err == nil {
		t.Fatal("missing persisted testcase was accepted")
	}
}

func TestProblemTestCaseResponseDecodesWithoutEnvelope(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte(`{"cases":[{"no":1,"input":"001.in","answer":"001.ans","score":100,"sample":true}]}`))
	}))
	defer server.Close()

	var response struct {
		Cases []composeProblemTestCase `json:"cases"`
	}
	if err := doJSONWithHeaders(context.Background(), http.MethodGet, server.URL, nil, nil, &response); err != nil {
		t.Fatalf("decode raw testcase response: %v", err)
	}
	want := composeProblemTestCase{No: 1, Input: "001.in", Answer: "001.ans", Score: 100, Sample: true}
	if err := validateComposeProblemTestCase(response.Cases, want); err != nil {
		t.Fatalf("validate decoded testcase response: %v", err)
	}
}

func TestProblemContentObjectKeyMatchesProblemStorageContract(t *testing.T) {
	tests := []struct {
		name    string
		content string
		want    string
	}{
		{
			name:    "input",
			content: "1 1\n",
			want:    "problem-17-objects-sha256-3f11ad6bbc7ecca0b2416b713dee77f1a635c00aaeaa946e14cde1c2bfae56d5",
		},
		{
			name:    "answer",
			content: "ok\n",
			want:    "problem-17-objects-sha256-dc51b8c96c2d745df3bd5590d990230a482fd247123599548e0632fdbf97fc22",
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if got := problemContentObjectKey(17, []byte(test.content)); got != test.want {
				t.Fatalf("content object key = %q, want %q", got, test.want)
			}
		})
	}
}

func TestStorageObjectMatchesExactRejectsExtraBytes(t *testing.T) {
	var requestPath string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requestPath = r.URL.Path
		if r.Header.Get("Authorization") != "Bearer service-token" ||
			r.Header.Get("X-OJOS-Caller-Service") != judgeAPIService {
			t.Errorf("missing storage service identity headers: %#v", r.Header)
		}
		_, _ = w.Write([]byte("1 1\nextra"))
	}))
	defer server.Close()

	headers := map[string]string{
		"Authorization":         "Bearer service-token",
		"X-OJOS-Caller-Service": judgeAPIService,
	}
	matched, err := storageObjectMatchesExact(
		context.Background(),
		server.URL+"/internal/apis/storage.object.get/problems/object-key",
		headers,
		"problems",
		"object-key",
		[]byte("1 1\n"),
	)
	if err == nil || matched {
		t.Fatalf("extra object bytes were accepted: matched=%v err=%v", matched, err)
	}
	if requestPath != "/internal/apis/storage.object.get/problems/object-key" {
		t.Fatalf("storage request path = %q", requestPath)
	}
}

func TestComposeDockerArgsMatchDrillOrdering(t *testing.T) {
	t.Setenv("OJOS_COMPOSE_ENV_FILE", "")
	t.Setenv("OJOS_COMPOSE_DEV_OVERRIDE", "")
	repoRoot := filepath.Join("test", "repo")
	want := []string{
		"compose",
		"--profile", "legacy-development",
		"--env-file", filepath.Join(repoRoot, ".env.example"),
		"-f", filepath.Join(repoRoot, "deploy", "compose", "docker-compose.yml"),
		"-f", filepath.Join(repoRoot, "deploy", "compose", "docker-compose.dev.yml"),
		"ps", "--format", "json",
	}
	if got := composeDockerArgs(repoRoot, "ps", "--format", "json"); !reflect.DeepEqual(got, want) {
		t.Fatalf("compose args = %#v, want %#v", got, want)
	}
}

func TestComposeDockerArgsHonorEnvFileAndDevOverride(t *testing.T) {
	t.Setenv("OJOS_COMPOSE_ENV_FILE", filepath.Join("custom", "compose.env"))
	t.Setenv("OJOS_COMPOSE_DEV_OVERRIDE", filepath.Join("custom", "docker-compose.ci.yml"))
	repoRoot := filepath.Join("test", "repo")
	want := []string{
		"compose",
		"--profile", "legacy-development",
		"--env-file", filepath.Join("custom", "compose.env"),
		"-f", filepath.Join(repoRoot, "deploy", "compose", "docker-compose.yml"),
		"-f", filepath.Join("custom", "docker-compose.ci.yml"),
		"run", "--rm", "judge-api-migrations",
	}
	if got := composeDockerArgs(repoRoot, "run", "--rm", "judge-api-migrations"); !reflect.DeepEqual(got, want) {
		t.Fatalf("compose args = %#v, want %#v", got, want)
	}
}

func TestComposeSmokePushedRouteTableCoversLiveJudgeChain(t *testing.T) {
	endpoints := map[string]composeSmokeServiceEndpoint{
		authService: {
			host:             "172.20.0.9",
			port:             8081,
			providerEndpoint: "172.20.0.9:8081:auth-service",
		},
		storageService: {
			host:             "172.20.0.10",
			port:             8085,
			providerEndpoint: "172.20.0.10:8085:storage-service",
		},
		problemService: {
			host:             "172.20.0.11",
			port:             8083,
			providerEndpoint: "172.20.0.11:8083:problem-service",
		},
		judgeAPIService: {
			host:             "172.20.0.12",
			port:             8082,
			providerEndpoint: "172.20.0.12:8082:judge-api",
		},
	}
	generatedAt := time.Date(2026, time.August, 13, 1, 2, 3, 4, time.UTC)

	request, err := composeSmokePushedRouteTable(endpoints, generatedAt)
	if err != nil {
		t.Fatalf("compose smoke route table: %v", err)
	}
	if !request.PushedRouteTable || !request.CanProxy || request.NodeID != childNodeID {
		t.Fatalf("invalid pushed route table envelope: %#v", request)
	}
	if request.GeneratedAt != generatedAt.Format(time.RFC3339Nano) {
		t.Fatalf("generated_at = %q", request.GeneratedAt)
	}
	if len(request.Routes) != 8 {
		t.Fatalf("routes = %d, want 8", len(request.Routes))
	}

	auth := findComposeGatewayRoute(request.Routes, "auth.user.permission.check")
	if auth == nil || auth.ProviderService != authService || auth.ProviderNodeID != rootNodeID ||
		auth.UpstreamBase != "http://172.20.0.9:8081" || auth.Prefix != "/auth/admin/permission-check" ||
		auth.AuthMode != "service" || auth.RequiredPermission != "auth.permission.check" ||
		len(auth.Methods) != 1 || auth.Methods[0] != http.MethodPost || !auth.ProxyEnabled {
		t.Fatalf("invalid delegated permission route: %#v", auth)
	}

	wantStorage := map[string]struct {
		method     string
		permission string
	}{
		"storage.object.put":    {method: http.MethodPut, permission: "storage.object.write"},
		"storage.object.get":    {method: http.MethodGet, permission: "storage.object.read"},
		"storage.object.head":   {method: http.MethodHead, permission: "storage.object.read"},
		"storage.object.delete": {method: http.MethodDelete, permission: "storage.object.delete"},
	}
	for apiID, want := range wantStorage {
		route := findComposeGatewayRoute(request.Routes, apiID)
		if route == nil {
			t.Fatalf("missing storage route %s", apiID)
		}
		if route.ProviderService != storageService || route.ProviderNodeID != rootNodeID ||
			route.UpstreamBase != "http://172.20.0.10:8085" || route.Prefix != "/api/storage/objects" ||
			route.AuthMode != "service" || route.RequiredPermission != want.permission ||
			len(route.Methods) != 1 || route.Methods[0] != want.method || !route.ProxyEnabled ||
			route.ServiceStatus != "RUNNING" {
			t.Fatalf("invalid storage route %s: %#v", apiID, route)
		}
	}

	problem := findComposeGatewayServiceRoute(request.Routes, problemService)
	if problem == nil || problem.Prefix != "/api/problem" || problem.StripPrefix != "/api/problem" ||
		problem.UpstreamBase != "http://172.20.0.11:8083" || problem.AuthMode != "user" {
		t.Fatalf("invalid problem route: %#v", problem)
	}
	judge := findComposeGatewayServiceRoute(request.Routes, judgeAPIService)
	if judge == nil || judge.Prefix != "/api/judge" || judge.StripPrefix != "/api/judge" ||
		judge.RewritePrefix != "/judge" || judge.UpstreamBase != "http://172.20.0.12:8082" ||
		judge.AuthMode != "user" || judge.RequiredPermission != "judge.submission.view.own" {
		t.Fatalf("invalid judge route: %#v", judge)
	}
	judgeQueue := findComposeGatewayRouteByID(request.Routes, "compose-smoke:judge-api-admin-queue")
	if judgeQueue == nil || judgeQueue.RouteID != "compose-smoke:judge-api-admin-queue" ||
		judgeQueue.APIID != "" ||
		judgeQueue.ProviderService != judgeAPIService || judgeQueue.ProviderNodeID != childNodeID ||
		judgeQueue.Prefix != "/api/judge/admin/queue" || judgeQueue.StripPrefix != "/api/judge/admin/queue" ||
		judgeQueue.RewritePrefix != "/judge/admin/queue" || judgeQueue.AuthMode != "user" ||
		judgeQueue.RequiredPermission != "judge.admin" || len(judgeQueue.Methods) != 1 ||
		judgeQueue.Methods[0] != http.MethodGet || !judgeQueue.ProxyEnabled {
		t.Fatalf("invalid judge queue admin route: %#v", judgeQueue)
	}
	if judgeQueue.Priority <= judge.Priority {
		t.Fatalf("judge queue route priority = %d, wildcard priority = %d", judgeQueue.Priority, judge.Priority)
	}
}

func TestComposeSmokePushedRouteTableRejectsMissingEndpoint(t *testing.T) {
	_, err := composeSmokePushedRouteTable(map[string]composeSmokeServiceEndpoint{}, time.Now())
	if err == nil {
		t.Fatal("missing compose endpoints were accepted")
	}
}

func findComposeGatewayRoute(routes []composeGatewayRoute, apiID string) *composeGatewayRoute {
	for i := range routes {
		if routes[i].APIID == apiID {
			return &routes[i]
		}
	}
	return nil
}

func findComposeGatewayRouteByID(routes []composeGatewayRoute, routeID string) *composeGatewayRoute {
	for i := range routes {
		if routes[i].RouteID == routeID {
			return &routes[i]
		}
	}
	return nil
}

func findComposeGatewayServiceRoute(routes []composeGatewayRoute, service string) *composeGatewayRoute {
	for i := range routes {
		if routes[i].RouteID == "compose-smoke:"+service && routes[i].APIID == "" && routes[i].ServiceID == service {
			return &routes[i]
		}
	}
	return nil
}
