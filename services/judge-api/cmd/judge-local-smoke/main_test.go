package main

import (
	"net/http"
	"path/filepath"
	"reflect"
	"testing"
	"time"
)

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
	if len(request.Routes) != 6 {
		t.Fatalf("routes = %d, want 6", len(request.Routes))
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
		judge.RewritePrefix != "/judge" || judge.UpstreamBase != "http://172.20.0.12:8082" || judge.AuthMode != "user" {
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
