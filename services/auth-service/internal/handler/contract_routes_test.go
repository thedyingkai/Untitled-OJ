package handler

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"

	"ojos-auth-service/internal/svc"

	"github.com/zeromicro/go-zero/core/service"
	"github.com/zeromicro/go-zero/rest"
)

func TestRegisteredV3RoutesMatchGeneratedServerAdapter(t *testing.T) {
	server, err := rest.NewServer(rest.RestConf{
		Host: "127.0.0.1", Port: 0,
		ServiceConf: service.ServiceConf{Name: "auth-service-contract-test"},
	})
	if err != nil {
		t.Fatal(err)
	}
	identity := func(next http.HandlerFunc) http.HandlerFunc { return next }
	RegisterHandlers(server, &svc.ServiceContext{
		AuthMiddleware: identity, WorkloadControlPlaneMiddleware: identity,
		DelegatedPermissionMiddleware: identity,
	})
	want := []string{
		"DELETE /api/v1/topologies/:id",
		"GET /api/v1/topologies/:id",
		"GET /auth/.well-known/workload-jwks.json",
		"GET /auth/admin/audit-logs",
		"GET /auth/admin/permission-assignments",
		"GET /auth/admin/permissions",
		"GET /auth/admin/resource-edges",
		"GET /auth/admin/resource-types",
		"GET /auth/admin/role-bindings",
		"GET /auth/admin/roles",
		"GET /auth/admin/services/:service_code/identity",
		"GET /auth/admin/users",
		"GET /auth/admin/users/:user_id/effective-permissions",
		"GET /auth/me",
		"GET /auth/profile",
		"GET /healthz",
		"GET /readyz",
		"POST /auth/admin/permission-assignments",
		"POST /auth/admin/permission-check",
		"POST /auth/admin/permissions",
		"POST /auth/admin/problems/roles",
		"POST /auth/admin/resource-edges",
		"POST /auth/admin/resource-types",
		"POST /auth/admin/role-bindings",
		"POST /auth/admin/role-permissions",
		"POST /auth/admin/roles",
		"POST /auth/admin/services/:service_code/credentials",
		"POST /auth/admin/services/:service_code/credentials/revoke",
		"POST /auth/admin/services/:service_code/permissions",
		"POST /auth/admin/users/roles",
		"POST /auth/internal/workload-tokens:issue",
		"POST /auth/login",
		"POST /auth/permission-check",
		"POST /auth/register",
		"PUT /api/v1/topologies/:id",
		"DELETE /auth/admin/permission-assignments",
		"DELETE /auth/admin/permissions",
		"DELETE /auth/admin/problems/roles",
		"DELETE /auth/admin/resource-edges",
		"DELETE /auth/admin/resource-types",
		"DELETE /auth/admin/role-bindings",
		"DELETE /auth/admin/role-permissions",
		"DELETE /auth/admin/roles",
		"DELETE /auth/admin/services/:service_code/permissions",
		"DELETE /auth/admin/users/roles",
	}
	if (&svc.ServiceContext{}).AdminBootstrap != nil {
		want = append(want, "POST /auth/bootstrap/admin")
	}
	registered := make([]string, 0, len(want))
	for _, route := range server.Routes() {
		key := route.Method + " " + route.Path
		for _, expected := range want {
			if key == expected {
				registered = append(registered, key)
			}
		}
	}
	sort.Strings(registered)
	sort.Strings(want)
	if len(registered) != len(want) {
		t.Fatalf("registered v3 routes = %v, want %v", registered, want)
	}
	for index := range want {
		if registered[index] != want[index] {
			t.Fatalf("registered v3 routes = %v, want %v", registered, want)
		}
	}
}

func TestGeneratedAdapterMatchesRegisteredV3Routes(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("..", "..", "gen", "gozero", "server-adapter.json"))
	if err != nil {
		t.Fatal(err)
	}
	var adapter struct {
		Operations []struct {
			Method string `json:"method"`
			Path   string `json:"path"`
		} `json:"operations"`
	}
	if err := json.Unmarshal(data, &adapter); err != nil {
		t.Fatal(err)
	}
	server, err := rest.NewServer(rest.RestConf{
		Host: "127.0.0.1", Port: 0,
		ServiceConf: service.ServiceConf{Name: "auth-service-adapter-test"},
	})
	if err != nil {
		t.Fatal(err)
	}
	identity := func(next http.HandlerFunc) http.HandlerFunc { return next }
	RegisterHandlers(server, &svc.ServiceContext{
		AuthMiddleware: identity, WorkloadControlPlaneMiddleware: identity,
		DelegatedPermissionMiddleware: identity,
	})
	registered := make(map[string]struct{})
	for _, route := range server.Routes() {
		registered[route.Method+" "+route.Path] = struct{}{}
	}
	for _, operation := range adapter.Operations {
		path := strings.ReplaceAll(operation.Path, "{", ":")
		path = strings.ReplaceAll(path, "}", "")
		key := operation.Method + " " + path
		if key == "POST /auth/bootstrap/admin" {
			continue
		}
		if _, ok := registered[key]; !ok {
			t.Errorf("generated operation %s has no registered handler", key)
		}
	}
}
