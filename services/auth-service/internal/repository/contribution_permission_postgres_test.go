package repository

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
)

func TestContributionPermissionProjectionPostgresPreservesAssignments(t *testing.T) {
	databaseURL := os.Getenv("AUTH_CONTRIBUTION_TEST_DATABASE_URL")
	if databaseURL == "" {
		databaseURL = os.Getenv("AUTH_BOOTSTRAP_TEST_DATABASE_URL")
	}
	if databaseURL == "" {
		t.Skip("AUTH_CONTRIBUTION_TEST_DATABASE_URL is not configured")
	}
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	adminPool, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	defer adminPool.Close()
	schema := fmt.Sprintf("auth_contribution_test_%d", time.Now().UnixNano())
	if _, err := adminPool.Exec(ctx, "CREATE SCHEMA "+schema); err != nil {
		t.Fatal(err)
	}
	defer func() { _, _ = adminPool.Exec(context.Background(), "DROP SCHEMA "+schema+" CASCADE") }()
	config, err := pgxpool.ParseConfig(databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	config.ConnConfig.RuntimeParams["search_path"] = schema
	pool, err := pgxpool.NewWithConfig(ctx, config)
	if err != nil {
		t.Fatal(err)
	}
	defer pool.Close()
	for _, migration := range []string{
		"000001_init_schema.up.sql",
		"000003_permission_core.up.sql",
		"000015_contribution_permission_projection.up.sql",
	} {
		contents, err := os.ReadFile(filepath.Join("..", "..", "migrations", migration))
		if err != nil {
			t.Fatal(err)
		}
		if _, err := pool.Exec(ctx, string(contents)); err != nil {
			t.Fatalf("apply %s: %v", migration, err)
		}
	}

	repository := NewAdminRepository(pool)
	firstDigest := "sha256:" + stringsRepeat("a", 64)
	if err := repository.ReconcileContributionPermissions(ctx, firstDigest, []ContributionPermissionDefinitionInput{{
		Code: "contest.read", ServiceCode: "contest-service", Title: "Read contests",
	}}); err != nil {
		t.Fatal(err)
	}
	if _, err := pool.Exec(ctx, `
INSERT INTO roles(name, service_code, description, is_system)
VALUES('contest-reader', 'contest-service', '', FALSE);
INSERT INTO role_permissions(role_id, permission_code)
SELECT id, 'contest.read' FROM roles WHERE name = 'contest-reader';
INSERT INTO permission_assignments(principal_type, principal_id, permission_code, scope_type, scope_id, effect)
VALUES('user', 42, 'contest.read', 'system', 0, 'allow');
`); err != nil {
		t.Fatal(err)
	}
	secondDigest := "sha256:" + stringsRepeat("b", 64)
	if err := repository.ReconcileContributionPermissions(ctx, secondDigest, nil); err != nil {
		t.Fatal(err)
	}
	var active bool
	var roles, assignments int
	if err := pool.QueryRow(ctx, `SELECT active FROM contribution_permission_definitions WHERE permission_code='contest.read'`).Scan(&active); err != nil {
		t.Fatal(err)
	}
	if err := pool.QueryRow(ctx, `SELECT count(*) FROM role_permissions WHERE permission_code='contest.read'`).Scan(&roles); err != nil {
		t.Fatal(err)
	}
	if err := pool.QueryRow(ctx, `SELECT count(*) FROM permission_assignments WHERE permission_code='contest.read'`).Scan(&assignments); err != nil {
		t.Fatal(err)
	}
	if active || roles != 1 || assignments != 1 {
		t.Fatalf("definition retirement changed authorization relationships: active=%v roles=%d assignments=%d", active, roles, assignments)
	}
}

func stringsRepeat(value string, count int) string {
	result := ""
	for range count {
		result += value
	}
	return result
}
