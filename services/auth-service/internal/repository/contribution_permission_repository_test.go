package repository

import (
	"os"
	"strings"
	"testing"
)

func TestContributionPermissionProjectionNeverDeletesAssignments(t *testing.T) {
	source, err := os.ReadFile("admin_repository.go")
	if err != nil {
		t.Fatal(err)
	}
	text := string(source)
	start := strings.Index(text, "func (r *AdminRepository) ReconcileContributionPermissions")
	end := strings.Index(text[start:], "\nfunc canonicalSHA256")
	if start < 0 || end < 0 {
		t.Fatal("Contribution reconciliation implementation is missing")
	}
	implementation := text[start : start+end]
	for _, forbidden := range []string{"DELETE FROM permissions", "DELETE FROM role_permissions", "DELETE FROM permission_assignments"} {
		if strings.Contains(implementation, forbidden) {
			t.Fatalf("Contribution definition reconciliation mutates authorization relationships: %s", forbidden)
		}
	}
	if !strings.Contains(implementation, "active = FALSE") {
		t.Fatal("retired definitions are not marked inactive")
	}
}

func TestContributionProjectionMigrationPreservesPermissionForeignKeys(t *testing.T) {
	source, err := os.ReadFile("../../migrations/000015_contribution_permission_projection.up.sql")
	if err != nil {
		t.Fatal(err)
	}
	text := string(source)
	if !strings.Contains(text, "REFERENCES permissions(code) ON DELETE RESTRICT") {
		t.Fatal("Contribution definition projection must preserve referenced permissions")
	}
}
