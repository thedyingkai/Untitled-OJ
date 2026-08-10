package repository

import (
	"os"
	"strings"
	"testing"
)

// The repository uses pgxpool directly, so its concurrent transaction behavior
// is covered by the PostgreSQL integration gate. This test keeps the critical
// single-winner and durability clauses from being weakened by a generated-code
// rewrite without adding a mock SQL implementation that behaves unlike PG.
func TestAdminBootstrapRepositoryKeepsTransactionalSingleWinnerContract(t *testing.T) {
	data, err := os.ReadFile("admin_bootstrap_repository.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, required := range []string{
		"func (r *AdminBootstrapRepository) ValidateState",
		"pgx.Serializable",
		"FOR UPDATE",
		"completed_at IS NULL",
		"ErrAdminBootstrapConsumed",
		"existingAdministratorID",
		"auth.bootstrap.detect_existing_admin",
		"auth.bootstrap.initial_admin",
		"tx.Commit(ctx)",
	} {
		if !strings.Contains(source, required) {
			t.Fatalf("admin bootstrap transaction lost required clause %q", required)
		}
	}
}

func TestAdminBootstrapMigrationPermanentlyConsumesUpgradedAdministrator(t *testing.T) {
	data, err := os.ReadFile("../../migrations/000014_initial_admin_bootstrap.up.sql")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, required := range []string{
		"WITH existing_admin AS",
		"JOIN user_roles",
		"JOIN role_bindings",
		"SET completed_at = NOW(), user_id = existing_admin.user_id",
	} {
		if !strings.Contains(source, required) {
			t.Fatalf("upgrade migration can reopen bootstrap; missing %q", required)
		}
	}
}
