package handler

import (
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"

	"ojos-judge-api/internal/svc"

	"github.com/zeromicro/go-zero/core/service"
	"github.com/zeromicro/go-zero/rest"

	"gopkg.in/yaml.v2"
)

type routeContractDocument struct {
	Paths map[string]map[string]any `yaml:"paths"`
}

func TestManagedRuntimeDoesNotRegisterLegacyWorkerPrefix(t *testing.T) {
	server, err := rest.NewServer(rest.RestConf{
		Host: "127.0.0.1", Port: 0,
		ServiceConf: service.ServiceConf{Name: "judge-managed-route-test"},
	})
	if err != nil {
		t.Fatal(err)
	}
	identity := func(next http.HandlerFunc) http.HandlerFunc { return next }
	RegisterHandlers(server, &svc.ServiceContext{
		Managed:               true,
		UserContextMiddleware: identity,
		WorkerAuthMiddleware:  identity,
	})
	for _, route := range server.Routes() {
		if strings.HasPrefix(route.Path, "/judge/worker/") {
			t.Fatalf("managed runtime registered legacy worker route %s %s", route.Method, route.Path)
		}
	}
}

func TestRuntimeRegistersEveryAuthoritativeProviderPath(t *testing.T) {
	apiRoot := filepath.Join("..", "..", "api")
	want := map[string]bool{}
	entries, err := os.ReadDir(apiRoot)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".openapi.yaml") {
			continue
		}
		data, err := os.ReadFile(filepath.Join(apiRoot, entry.Name()))
		if err != nil {
			t.Fatal(err)
		}
		var document routeContractDocument
		if err := yaml.Unmarshal(data, &document); err != nil {
			t.Fatalf("parse %s: %v", entry.Name(), err)
		}
		for path, methods := range document.Paths {
			for method := range methods {
				switch strings.ToUpper(method) {
				case "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS":
					want[strings.ToUpper(method)+" "+path] = true
				}
			}
		}
	}

	routeSource, err := os.ReadFile("routes.go")
	if err != nil {
		t.Fatal(err)
	}
	registered := registeredRuntimeRoutes(string(routeSource))
	for route := range want {
		if !registered[route] {
			t.Errorf("OpenAPI provider route %s is not registered by the runtime", route)
		}
	}
}

func registeredRuntimeRoutes(source string) map[string]bool {
	result := map[string]bool{
		"GET /healthz": true,
		"GET /readyz":  true,
	}
	methodExpression := regexp.MustCompile(`Method:\s+http\.Method([A-Za-z]+),\s+Path:\s+"([^"]+)"`)
	for _, match := range methodExpression.FindAllStringSubmatch(source, -1) {
		method := strings.ToUpper(match[1])
		path := strings.ReplaceAll(match[2], ":", "{")
		if strings.Contains(path, "{") {
			parts := strings.Split(path, "/")
			for index, part := range parts {
				if strings.HasPrefix(part, "{") && !strings.HasSuffix(part, "}") {
					parts[index] = part + "}"
				}
			}
			path = strings.Join(parts, "/")
		}
		// User/admin routes are all registered below /judge.
		result[method+" /judge"+path] = true
		// Worker routes have both a historical /judge prefix and the
		// authoritative provider-native /api/judge prefix.
		if strings.HasPrefix(path, "/worker/") {
			result[method+" /api/judge"+path] = true
		}
	}
	return result
}
