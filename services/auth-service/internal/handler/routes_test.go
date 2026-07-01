package handler

import (
	"os"
	"strings"
	"testing"
)

func TestRoutesExposeReleasePermissionEndpoints(t *testing.T) {
	data, err := os.ReadFile("routes.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, want := range []string{
		`Path:    "/admin/services/:service_code/permissions"`,
		`Handler: registerServicePermissionsHandler(serverCtx)`,
		`Handler: deleteServicePermissionsHandler(serverCtx)`,
		`Path:    "/admin/users/:user_id/effective-permissions"`,
		`Handler: userEffectivePermissionsHandler(serverCtx)`,
		`Path:    "/permission-check"`,
		`Handler: userPermissionCheckHandler(serverCtx)`,
	} {
		if !strings.Contains(source, want) {
			t.Fatalf("routes.go missing %s", want)
		}
	}
}
