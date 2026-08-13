package handler

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ojos-gateway/internal/svc"
)

type gatewayContractOperation struct {
	Audience        string `json:"audience"`
	Auth            string `json:"auth"`
	Method          string `json:"method"`
	OperationID     string `json:"operationId"`
	Permission      string `json:"permission"`
	PermissionScope any    `json:"permissionScope"`
	ProviderPath    string `json:"providerPath"`
}

func TestGeneratedContractMatchesReservedPlatformRoutes(t *testing.T) {
	payload, err := os.ReadFile(filepath.Join("..", "..", "gen", "service.contract.json"))
	if err != nil {
		t.Fatal(err)
	}
	var contract struct {
		Operations      []gatewayContractOperation `json:"operations"`
		APIRequirements []struct {
			ID       string `json:"id"`
			Optional bool   `json:"optional"`
		} `json:"apiRequirements"`
		Routes               []json.RawMessage `json:"routes"`
		Exposures            []json.RawMessage `json:"exposures"`
		Frontends            []json.RawMessage `json:"frontends"`
		Permissions          []json.RawMessage `json:"permissions"`
		PermissionReferences []string          `json:"permissionReferences"`
		ResourceClaims       []json.RawMessage `json:"resourceClaims"`
		Migrations           []json.RawMessage `json:"migrations"`
		Runtime              struct {
			Profile string `json:"profile"`
			Health  struct {
				Path string `json:"path"`
			} `json:"health"`
		} `json:"runtime"`
	}
	if err := json.Unmarshal(payload, &contract); err != nil {
		t.Fatal(err)
	}
	if len(contract.Routes) != 0 || len(contract.Exposures) != 0 || len(contract.Frontends) != 0 {
		t.Fatalf("platform Gateway must not contribute self-proxy routes or modules: routes=%d exposures=%d frontends=%d", len(contract.Routes), len(contract.Exposures), len(contract.Frontends))
	}
	if len(contract.Permissions) != 0 || len(contract.PermissionReferences) != 1 || contract.PermissionReferences[0] != "system.admin" {
		t.Fatalf("Gateway must reference, not own, system.admin: permissions=%v references=%v", contract.Permissions, contract.PermissionReferences)
	}
	if len(contract.APIRequirements) != 1 || contract.APIRequirements[0].ID != "auth.user.permission.check" || contract.APIRequirements[0].Optional {
		t.Fatalf("Gateway permission ApiBinding is not exact and required: %+v", contract.APIRequirements)
	}
	if len(contract.ResourceClaims) != 0 || len(contract.Migrations) != 0 {
		t.Fatalf("Gateway must not claim an owned database or migration: resources=%d migrations=%d", len(contract.ResourceClaims), len(contract.Migrations))
	}
	if contract.Runtime.Profile != "standard-container-v1" || contract.Runtime.Health.Path != "/readyz" {
		t.Fatalf("Gateway runtime baseline is invalid: %+v", contract.Runtime)
	}

	runtime := make(map[string]struct{})
	for _, route := range platformRoutes(&svc.ServiceContext{}) {
		path := strings.ReplaceAll(route.Path, ":id", "{id}")
		runtime[route.Method+" "+path] = struct{}{}
	}
	// /metrics is installed once by shared observability middleware in main.
	runtime[http.MethodGet+" /metrics"] = struct{}{}
	if len(contract.Operations) != len(runtime) {
		t.Fatalf("contract operation count=%d, runtime platform route count=%d", len(contract.Operations), len(runtime))
	}
	for _, operation := range contract.Operations {
		if operation.Audience != "internal" {
			t.Fatalf("platform operation %s leaked to Contribution audience %q", operation.OperationID, operation.Audience)
		}
		if _, ok := runtime[operation.Method+" "+operation.ProviderPath]; !ok {
			t.Fatalf("signed operation %s %s has no reserved runtime handler", operation.Method, operation.ProviderPath)
		}
		if strings.HasPrefix(operation.ProviderPath, "/api/admin/") {
			if operation.Auth != "required" || operation.Permission != "system.admin" || operation.PermissionScope != "system" {
				t.Fatalf("admin operation %s does not exactly require system.admin/system: %+v", operation.OperationID, operation)
			}
		}
	}
}
