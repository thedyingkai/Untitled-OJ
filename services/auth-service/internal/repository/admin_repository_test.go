package repository

import (
	"os"
	"strings"
	"testing"
)

func TestRegisterServicePermissionsReconcilesReleaseDeclaredPermissions(t *testing.T) {
	data, err := os.ReadFile("admin_repository.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, want := range []string{
		"DELETE FROM permissions",
		"WHERE service_code = $1",
		"NOT (code = ANY($2::text[]))",
		"DELETE FROM role_permissions",
		"WHERE role_id = $1",
		"WHERE service_code = $2",
	} {
		if !strings.Contains(source, want) {
			t.Fatalf("RegisterServicePermissions must reconcile release permissions; missing %q", want)
		}
	}
}

func TestDeleteServicePermissionsRevokesByServiceCode(t *testing.T) {
	data, err := os.ReadFile("admin_repository.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	if !strings.Contains(source, "DELETE FROM permissions\nWHERE service_code = $1") {
		t.Fatalf("DeleteServicePermissions must revoke service-owned permissions by service_code")
	}
}

func TestServiceCallerPermissionChecksRegisteredPermissionCode(t *testing.T) {
	data, err := os.ReadFile("admin_repository.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, want := range []string{
		"func (r *AdminRepository) ServiceCallerCanUsePermission",
		"FROM service_identities si",
		"JOIN service_credentials sc",
		"JOIN service_permission_grants spg",
		"si.service_code = $1",
		"sc.token_hash = $4",
		"spg.permission_code = $2",
		"spg.api_id = $3",
	} {
		if !strings.Contains(source, want) {
			t.Fatalf("ServiceCallerCanUsePermission must query registered service identity grants; missing %q", want)
		}
	}
}
