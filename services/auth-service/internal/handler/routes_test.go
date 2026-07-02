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
		`Path:    "/admin/services/:service_code/identity"`,
		`Handler: getServiceIdentityHandler(serverCtx)`,
		`Path:    "/admin/services/:service_code/credentials"`,
		`Handler: addServiceCredentialHandler(serverCtx)`,
		`Path:    "/admin/services/:service_code/credentials/revoke"`,
		`Handler: revokeServiceCredentialHandler(serverCtx)`,
		`Path:    "/admin/roles"`,
		`Handler: upsertRoleHandler(serverCtx)`,
		`Handler: deleteRoleHandler(serverCtx)`,
		`Path:    "/admin/role-permissions"`,
		`Handler: grantRolePermissionHandler(serverCtx)`,
		`Handler: revokeRolePermissionHandler(serverCtx)`,
		`Path:    "/admin/permissions"`,
		`Handler: upsertPermissionHandler(serverCtx)`,
		`Handler: deletePermissionHandler(serverCtx)`,
		`Path:    "/admin/resource-types"`,
		`Handler: upsertResourceTypeHandler(serverCtx)`,
		`Handler: deleteResourceTypeHandler(serverCtx)`,
		`Path:    "/admin/role-bindings"`,
		`Handler: bindRoleHandler(serverCtx)`,
		`Handler: listRoleBindingsHandler(serverCtx)`,
		`Handler: unbindRoleHandler(serverCtx)`,
		`Path:    "/admin/permission-assignments"`,
		`Handler: assignPermissionHandler(serverCtx)`,
		`Handler: listPermissionAssignmentsHandler(serverCtx)`,
		`Handler: revokePermissionHandler(serverCtx)`,
		`Path:    "/admin/resource-edges"`,
		`Handler: addResourceEdgeHandler(serverCtx)`,
		`Handler: listResourceEdgesHandler(serverCtx)`,
		`Handler: removeResourceEdgeHandler(serverCtx)`,
		`Path:    "/admin/resource-types"`,
		`Handler: listResourceTypesHandler(serverCtx)`,
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
