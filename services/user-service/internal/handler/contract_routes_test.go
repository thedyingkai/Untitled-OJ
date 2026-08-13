package handler

import (
	"net/http"
	"sort"
	"testing"

	"ojos-user-service/internal/svc"

	"github.com/zeromicro/go-zero/core/service"
	"github.com/zeromicro/go-zero/rest"
)

func TestRegisteredV3RoutesMatchGeneratedServerAdapter(t *testing.T) {
	server, err := rest.NewServer(rest.RestConf{
		Host: "127.0.0.1",
		Port: 0,
		ServiceConf: service.ServiceConf{
			Name: "user-service-contract-test",
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	RegisterHandlers(server, &svc.ServiceContext{UserContextMiddleware: func(next http.HandlerFunc) http.HandlerFunc { return next }})

	want := []string{
		"GET /admin/users/:user_id/preferences",
		"GET /admin/users/:user_id/profile",
		"GET /admin/users/:user_id/stats",
		"GET /api/users/me",
		"GET /api/users/me/preferences",
		"GET /healthz",
		"GET /readyz",
		"PATCH /admin/users/:user_id/preferences",
		"PATCH /admin/users/:user_id/profile",
		"PATCH /api/users/me",
		"PATCH /api/users/me/preferences",
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

func TestManagedRuntimeDoesNotRegisterLegacyUserIDRoutes(t *testing.T) {
	server, err := rest.NewServer(rest.RestConf{
		Host: "127.0.0.1", Port: 0,
		ServiceConf: service.ServiceConf{Name: "user-service-managed-route-test"},
	})
	if err != nil {
		t.Fatal(err)
	}
	RegisterHandlers(server, &svc.ServiceContext{
		Managed:               true,
		UserContextMiddleware: func(next http.HandlerFunc) http.HandlerFunc { return next },
	})
	for _, route := range server.Routes() {
		if route.Path == "/api/users/:user_id/profile" || route.Path == "/api/users/:user_id/preferences" || route.Path == "/api/users/:user_id/stats" {
			t.Fatalf("managed runtime registered legacy route %s %s", route.Method, route.Path)
		}
	}
}
