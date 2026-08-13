package main

import (
	"context"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"reflect"
	"testing"
	"time"
)

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
	if len(request.Routes) != 7 {
		t.Fatalf("routes = %d, want 7", len(request.Routes))
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

func findComposeGatewayServiceRoute(routes []composeGatewayRoute, service string) *composeGatewayRoute {
	for i := range routes {
		if routes[i].APIID == "" && routes[i].ServiceID == service {
			return &routes[i]
		}
	}
	return nil
}
