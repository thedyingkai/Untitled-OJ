package handler

import (
	"os"
	"strings"
	"testing"
)

func TestAdminServiceTopologyRoutePrecedesDetailRoute(t *testing.T) {
	data, err := os.ReadFile("routes.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)

	sets := strings.Index(source, `"/api/admin/sets"`)
	topology := strings.Index(source, `"/api/admin/topology"`)
	runtimeSnapshot := strings.Index(source, `"/api/admin/runtime/snapshot"`)
	runtimeRoutes := strings.Index(source, `"/api/admin/runtime/routes"`)
	detail := strings.Index(source, `"/api/admin/services/:id"`)
	if sets < 0 {
		t.Fatalf("sets route not found")
	}
	if topology < 0 {
		t.Fatalf("topology route not found")
	}
	if detail < 0 {
		t.Fatalf("service detail route not found")
	}
	if runtimeSnapshot < 0 {
		t.Fatalf("runtime snapshot route not found")
	}
	if runtimeRoutes < 0 {
		t.Fatalf("runtime route table routes not found")
	}
	if topology > detail {
		t.Fatalf("topology route must be registered before service detail route")
	}
	if runtimeSnapshot > detail {
		t.Fatalf("runtime snapshot route must be registered before service detail route")
	}
	if runtimeRoutes > detail {
		t.Fatalf("runtime route table routes must be registered before service detail route")
	}
	if sets > detail {
		t.Fatalf("sets route must be registered before service detail route")
	}
}
