package handler

import (
	"os"
	"strings"
	"testing"
)

func TestAdminModuleTopologyRoutePrecedesDetailRoute(t *testing.T) {
	data, err := os.ReadFile("routes.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)

	topology := strings.Index(source, `"/api/admin/modules/topology"`)
	runtimeSnapshot := strings.Index(source, `"/api/admin/modules/runtime-snapshot"`)
	runtimeRoutes := strings.Index(source, `"/api/admin/modules/runtime/routes"`)
	runtimeReload := strings.Index(source, `"/api/admin/modules/runtime/reload"`)
	installer := strings.Index(source, `"/api/admin/modules/discover"`)
	enable := strings.Index(source, `"/api/admin/modules/:id/enable"`)
	detail := strings.Index(source, `"/api/admin/modules/:id"`)
	if topology < 0 {
		t.Fatalf("topology route not found")
	}
	if detail < 0 {
		t.Fatalf("module detail route not found")
	}
	if runtimeSnapshot < 0 {
		t.Fatalf("runtime-snapshot route not found")
	}
	if runtimeRoutes < 0 || runtimeReload < 0 {
		t.Fatalf("runtime route table routes not found")
	}
	if installer < 0 || enable < 0 {
		t.Fatalf("installer routes not found")
	}
	if topology > detail {
		t.Fatalf("topology route must be registered before module detail route")
	}
	if installer > detail {
		t.Fatalf("installer static routes must be registered before module detail route")
	}
	if runtimeSnapshot > detail {
		t.Fatalf("runtime-snapshot route must be registered before module detail route")
	}
	if runtimeRoutes > detail || runtimeReload > detail {
		t.Fatalf("runtime route table routes must be registered before module detail route")
	}
	if enable > detail {
		t.Fatalf("installer action routes must be registered before module detail route")
	}
}
