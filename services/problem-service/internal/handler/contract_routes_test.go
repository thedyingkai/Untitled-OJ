package handler

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ojos-problem-service/internal/svc"

	"github.com/zeromicro/go-zero/core/service"
	"github.com/zeromicro/go-zero/rest"
)

func TestPublishedOperationsHaveConcreteRuntimeRoutes(t *testing.T) {
	want := map[string]bool{
		http.MethodGet + " /admin/artifact-gc/intents":            false,
		http.MethodGet + " /admin/problems":                       false,
		http.MethodPost + " /admin/artifact-gc/intents/reconcile": false,
		http.MethodPost + " /admin/artifact-gc/intents/retry":     false,
		http.MethodPost + " /problems":                            false,
		http.MethodGet + " /problems":                             false,
		http.MethodGet + " /problems/:id":                         false,
		http.MethodPut + " /problems/:id":                         false,
		http.MethodDelete + " /problems/:id":                      false,
		http.MethodGet + " /problems/:id/package":                 false,
		http.MethodGet + " /problems/:id/package/cases":           false,
		http.MethodPost + " /problems/:id/package/validate":       false,
		http.MethodPost + " /problems/:id/test-cases":             false,
		http.MethodGet + " /problems/:id/test-cases":              false,
		http.MethodPut + " /problems/:id/test-cases/:case_no":     false,
		http.MethodDelete + " /problems/:id/test-cases/:case_no":  false,
	}
	for _, route := range problemOperationRoutes(&svc.ServiceContext{}) {
		key := route.Method + " " + route.Path
		if _, ok := want[key]; !ok {
			t.Fatalf("unexpected authoritative runtime route %s", key)
		}
		want[key] = true
	}
	for route, found := range want {
		if !found {
			t.Fatalf("authoritative runtime route %s is missing", route)
		}
	}
	legacyActions := map[string]bool{
		"/admin/artifact-gc/intents:reconcile": false,
		"/admin/artifact-gc/intents:retry":     false,
	}
	for _, route := range legacyProblemRoutes(&svc.ServiceContext{}) {
		if _, ok := legacyActions[route.Path]; ok {
			legacyActions[route.Path] = true
			continue
		}
		if _, ok := want[route.Method+" "+route.Path]; !ok {
			t.Fatalf("legacy route %s %s is not a v3 route or approved colon alias", route.Method, route.Path)
		}
	}
	for route, found := range legacyActions {
		if !found {
			t.Fatalf("legacy action alias %s is missing", route)
		}
	}
}

func TestManagedRuntimeDoesNotRegisterLegacyProblemPrefix(t *testing.T) {
	server, err := rest.NewServer(rest.RestConf{
		Host: "127.0.0.1", Port: 0,
		ServiceConf: service.ServiceConf{Name: "problem-managed-route-test"},
	})
	if err != nil {
		t.Fatal(err)
	}
	RegisterHandlers(server, &svc.ServiceContext{
		Managed:               true,
		UserContextMiddleware: func(next http.HandlerFunc) http.HandlerFunc { return next },
	})
	for _, route := range server.Routes() {
		if strings.HasPrefix(route.Path, "/problem/") {
			t.Fatalf("managed runtime registered legacy route %s %s", route.Method, route.Path)
		}
	}
}

func TestGeneratedContractProviderRoutesMatchAuthoritativeRuntimeRoutes(t *testing.T) {
	path := filepath.Join("..", "..", "gen", "service.contract.json")
	bytes, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var contract struct {
		Operations []struct {
			Audience     string `json:"audience"`
			Method       string `json:"method"`
			ProviderPath string `json:"providerPath"`
		} `json:"operations"`
		Routes []struct {
			Method       string `json:"method"`
			ProviderPath string `json:"providerPath"`
		} `json:"routes"`
	}
	if err := json.Unmarshal(bytes, &contract); err != nil {
		t.Fatal(err)
	}
	runtime := make(map[string]struct{})
	for _, route := range problemOperationRoutes(&svc.ServiceContext{}) {
		providerPath := strings.ReplaceAll(route.Path, ":case_no", "{case_no}")
		providerPath = strings.ReplaceAll(providerPath, ":id", "{id}")
		runtime[route.Method+" "+providerPath] = struct{}{}
	}
	for _, route := range contract.Routes {
		key := route.Method + " " + route.ProviderPath
		if _, ok := runtime[key]; !ok {
			t.Fatalf("signed provider route %s has no authoritative runtime handler", key)
		}
	}
	for _, operation := range contract.Operations {
		if operation.Audience != "internal" {
			continue
		}
		key := operation.Method + " " + operation.ProviderPath
		if key != http.MethodGet+" /healthz" && key != http.MethodGet+" /readyz" {
			t.Fatalf("unexpected internal operation route %s", key)
		}
	}
}
