package permission

import (
	"os"
	"strings"
	"testing"
)

func TestPermissionCoreExposesRevocationAndAuditHelpers(t *testing.T) {
	data, err := os.ReadFile("permission.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, want := range []string{
		"func UnbindRole(",
		"DELETE FROM role_bindings",
		"func RevokePermissionAssignment(",
		"DELETE FROM permission_assignments",
		"func RemoveResourceEdge(",
		"DELETE FROM resource_edges",
		"func RevokeRolePermission(",
		"DELETE FROM role_permissions",
		"func WriteAuditLog(",
	} {
		if !strings.Contains(source, want) {
			t.Fatalf("permission core missing %q", want)
		}
	}
}
