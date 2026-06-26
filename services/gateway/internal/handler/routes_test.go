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
	detail := strings.Index(source, `"/api/admin/modules/:id"`)
	if topology < 0 {
		t.Fatalf("topology route not found")
	}
	if detail < 0 {
		t.Fatalf("module detail route not found")
	}
	if topology > detail {
		t.Fatalf("topology route must be registered before module detail route")
	}
}
