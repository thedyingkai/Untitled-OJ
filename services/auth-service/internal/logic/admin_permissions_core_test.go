package logic

import (
	"os"
	"strings"
	"testing"
)

func TestAdminPermissionsLogicExposesGenericManagement(t *testing.T) {
	data, err := os.ReadFile("admin_permissions_logic.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, want := range []string{
		"func (l *AdminPermissionsLogic) UpsertRole",
		"func (l *AdminPermissionsLogic) DeleteRole",
		"func (l *AdminPermissionsLogic) ListResourceTypes",
		"func (l *AdminPermissionsLogic) ListRoleBindings",
		"func (l *AdminPermissionsLogic) ListPermissionAssignments",
		"func (l *AdminPermissionsLogic) ListResourceEdges",
		"func (l *AdminPermissionsLogic) GrantRolePermission",
		"permission.GrantRolePermission",
		"func (l *AdminPermissionsLogic) RevokeRolePermission",
		"permission.RevokeRolePermission",
		"func (l *AdminPermissionsLogic) UpsertPermission",
		"permission.RegisterPermission",
		"func (l *AdminPermissionsLogic) DeletePermission",
		"func (l *AdminPermissionsLogic) UpsertResourceType",
		"permission.RegisterResourceType",
		"func (l *AdminPermissionsLogic) DeleteResourceType",
		"func (l *AdminPermissionsLogic) BindRole",
		"permission.BindRole",
		"func (l *AdminPermissionsLogic) UnbindRole",
		"permission.UnbindRole",
		"func (l *AdminPermissionsLogic) AssignPermission",
		"permission.AssignPermission",
		"func (l *AdminPermissionsLogic) RevokePermission",
		"permission.RevokePermissionAssignment",
		"func (l *AdminPermissionsLogic) AddResourceEdge",
		"permission.AddResourceEdge",
		"func (l *AdminPermissionsLogic) RemoveResourceEdge",
		"permission.RemoveResourceEdge",
		"permission.WriteAuditLog",
	} {
		if !strings.Contains(source, want) {
			t.Fatalf("admin permission logic missing %q", want)
		}
	}
}

func TestServicePermissionsLogicUsesExplicitCredentialLifecycle(t *testing.T) {
	data, err := os.ReadFile("servicepermissionslogic.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, want := range []string{
		"CredentialToken",
		"CredentialExpiresAt",
		"credentialTokenFromRegistration",
		"parseOptionalRFC3339(req.ServiceIdentity.CredentialExpiresAt)",
		"AddServiceCredential",
		"RevokeServiceCredential",
		"GetServiceIdentity",
	} {
		if !strings.Contains(source, want) {
			t.Fatalf("service permission lifecycle missing %q", want)
		}
	}
}

func TestPermissionCheckAuditHelperExists(t *testing.T) {
	data, err := os.ReadFile("permission_audit.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, want := range []string{
		"func auditUserPermissionCheck",
		"user.permission_check.allow",
		"user.permission_check.deny",
		"admin.permission_check.allow",
		"admin.permission_check.deny",
		"permission.WriteAuditLog",
	} {
		if !strings.Contains(source, want) {
			t.Fatalf("permission check audit helper missing %q", want)
		}
	}
}
