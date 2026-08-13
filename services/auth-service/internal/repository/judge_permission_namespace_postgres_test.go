package repository

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
	sharedperm "ojos-shared/security/permission"
)

func TestJudgePermissionNamespacePostgresPreservesAuthorizationSemantics(t *testing.T) {
	databaseURL := os.Getenv("AUTH_JUDGE_PERMISSION_TEST_DATABASE_URL")
	if databaseURL == "" {
		databaseURL = os.Getenv("AUTH_BOOTSTRAP_TEST_DATABASE_URL")
	}
	if databaseURL == "" {
		t.Skip("AUTH_JUDGE_PERMISSION_TEST_DATABASE_URL is not configured")
	}

	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	adminPool, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	defer adminPool.Close()
	schema := fmt.Sprintf("auth_judge_permission_test_%d", time.Now().UnixNano())
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
	applyJudgePermissionMigration(t, ctx, pool, "000001_init_schema.up.sql")
	applyJudgePermissionMigration(t, ctx, pool, "000003_permission_core.up.sql")
	applyJudgePermissionMigration(t, ctx, pool, "000015_contribution_permission_projection.up.sql")

	if _, err := pool.Exec(ctx, `
INSERT INTO roles(name, service_code, description, is_system)
VALUES
    ('custom-view-all', 'judge-api', '', FALSE),
    ('custom-manage-pair', 'judge-api', '', FALSE),
    ('custom-rejudge-only', 'judge-api', '', FALSE);
INSERT INTO role_permissions(role_id, permission_code)
SELECT id, 'submission.view.all' FROM roles WHERE name = 'custom-view-all';
INSERT INTO role_permissions(role_id, permission_code)
SELECT id, permission_code
FROM roles
CROSS JOIN (VALUES ('submission.rejudge'), ('submission.delete')) AS permissions(permission_code)
WHERE name = 'custom-manage-pair';
INSERT INTO role_permissions(role_id, permission_code)
SELECT id, 'submission.rejudge' FROM roles WHERE name = 'custom-rejudge-only';

INSERT INTO permission_assignments(
    principal_type, principal_id, permission_code, scope_type, scope_id, effect, reason, expires_at
)
	VALUES
	    ('user', 700, 'submission.view.own', 'system', 0, 'deny', 'legacy own deny', NULL),
	    ('user', 700, 'submission.view.all', 'system', 0, 'allow', 'legacy all allow', NULL),
	    ('user', 701, 'submission.view.all', 'system', 0, 'allow', 'legacy all allow', NULL),
	    ('user', 702, 'submission.view.all', 'system', 0, 'deny', 'legacy all deny', NULL),
	    ('user', 703, 'submission.view.own', 'system', 0, 'deny', 'legacy own deny', NULL),
	    ('user', 704, 'submission.rejudge', 'system', 0, 'deny', 'legacy rejudge deny', NULL),
	    ('user', 705, 'submission.delete', 'system', 0, 'deny', 'legacy delete deny', NULL),
	    ('user', 706, 'submission.rejudge', 'system', 0, 'deny', 'legacy rejudge deny', NULL),
	    ('user', 706, 'submission.delete', 'system', 0, 'deny', 'legacy delete deny', NULL),
	    ('user', 707, 'submission.view.own', 'system', 0, 'deny', 'expired legacy deny', NOW() - INTERVAL '1 hour'),
	    ('user', 708, 'submission.view.own', 'system', 0, 'deny', 'permanent legacy deny', NULL),
	    ('user', 709, 'submission.view.own', 'system', 0, 'allow', 'permanent legacy allow', NULL),
	    ('user', 710, 'submission.view.own', 'system', 0, 'allow', 'permanent legacy allow', NULL);

INSERT INTO users(id, username, email, password_hash)
VALUES
    (800, 'judge-normal-user', 'judge-normal@example.test', 'test-only'),
    (801, 'judge-admin-user', 'judge-admin@example.test', 'test-only');
INSERT INTO user_roles(user_id, role_id)
SELECT 800, id FROM roles WHERE name = 'user';
INSERT INTO user_roles(user_id, role_id)
SELECT 801, id FROM roles WHERE name = 'admin';

INSERT INTO permissions(code, service_code, name, description)
VALUES
    ('judge.submission.view.own', 'judge-api', 'Contribution Own', ''),
    ('judge.submission.view.all', 'judge-api', 'Contribution All', ''),
    ('judge.submission.manage', 'judge-api', 'Contribution Manage', '');
INSERT INTO contribution_permission_projection(singleton, snapshot_digest)
VALUES(TRUE, 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');
INSERT INTO contribution_permission_definitions(permission_code, service_code, snapshot_digest, active)
SELECT code, 'judge-api', 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', TRUE
FROM permissions
WHERE code LIKE 'judge.submission.%';
INSERT INTO permission_assignments(
	    principal_type, principal_id, permission_code, scope_type, scope_id, effect, reason, expires_at
)
	VALUES
	    ('user', 999, 'judge.submission.view.own', 'system', 0, 'allow', 'pre-existing contribution authorization', NULL),
	    ('user', 703, 'judge.submission.view.own', 'system', 0, 'allow', 'pre-existing conflicting authorization', NULL),
	    ('user', 704, 'judge.submission.manage', 'system', 0, 'allow', 'pre-existing conflicting authorization', NULL),
	    ('user', 705, 'judge.submission.manage', 'system', 0, 'allow', 'pre-existing conflicting authorization', NULL),
	    ('user', 706, 'judge.submission.manage', 'system', 0, 'allow', 'pre-existing conflicting authorization', NULL),
	    ('user', 707, 'judge.submission.view.own', 'system', 0, 'allow', 'current allow survives expired legacy deny', NULL),
	    ('user', 708, 'judge.submission.view.own', 'system', 0, 'allow', 'expired current allow loses to permanent legacy deny', NOW() - INTERVAL '1 hour'),
	    ('user', 709, 'judge.submission.view.own', 'system', 0, 'deny', 'expired current deny loses to permanent legacy allow', NOW() - INTERVAL '1 hour'),
	    ('user', 710, 'judge.submission.view.own', 'system', 0, 'allow', 'finite current allow is extended by permanent legacy allow', NOW() + INTERVAL '1 hour');
`); err != nil {
		t.Fatal(err)
	}

	applyJudgePermissionMigration(t, ctx, pool, "000016_judge_permission_namespace.up.sql")
	var contributionDigest string
	var contributionActive bool
	if err := pool.QueryRow(ctx, `
SELECT snapshot_digest, active
FROM contribution_permission_definitions
WHERE permission_code = 'judge.submission.view.own'
`).Scan(&contributionDigest, &contributionActive); err != nil {
		t.Fatal(err)
	}
	if contributionDigest != "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" || !contributionActive {
		t.Fatalf("migration changed Contribution ownership: digest=%s active=%v", contributionDigest, contributionActive)
	}
	assertJudgeRolePermissions(t, ctx, pool, "user", []string{"judge.submission.view.own"})
	assertJudgeRolePermissions(t, ctx, pool, "admin", []string{
		"judge.submission.manage", "judge.submission.view.all", "judge.submission.view.own",
	})
	assertJudgeRolePermissions(t, ctx, pool, "super_admin", []string{
		"judge.submission.manage", "judge.submission.view.all", "judge.submission.view.own",
	})
	assertJudgeRolePermissions(t, ctx, pool, "problem_owner", []string{
		"judge.submission.view.all", "judge.submission.view.own",
	})
	assertJudgeRolePermissions(t, ctx, pool, "problem_setter", nil)
	assertJudgeRolePermissions(t, ctx, pool, "custom-view-all", []string{
		"judge.submission.view.all", "judge.submission.view.own",
	})
	assertJudgeRolePermissions(t, ctx, pool, "custom-manage-pair", []string{"judge.submission.manage"})
	assertJudgeRolePermissions(t, ctx, pool, "custom-rejudge-only", nil)

	assertJudgeDirectAssignment(t, ctx, pool, 700, "judge.submission.view.own", "deny", true)
	assertJudgeDirectAssignment(t, ctx, pool, 700, "judge.submission.view.all", "allow", true)
	assertJudgeDirectAssignment(t, ctx, pool, 701, "judge.submission.view.own", "allow", true)
	assertJudgeDirectAssignment(t, ctx, pool, 701, "judge.submission.view.all", "allow", true)
	assertJudgeDirectAssignment(t, ctx, pool, 702, "judge.submission.view.own", "", false)
	assertJudgeDirectAssignment(t, ctx, pool, 702, "judge.submission.view.all", "deny", true)
	assertJudgeDirectAssignment(t, ctx, pool, 703, "judge.submission.view.own", "deny", true)
	assertJudgeDirectAssignment(t, ctx, pool, 704, "judge.submission.manage", "deny", true)
	assertJudgeDirectAssignment(t, ctx, pool, 705, "judge.submission.manage", "deny", true)
	assertJudgeDirectAssignment(t, ctx, pool, 706, "judge.submission.manage", "deny", true)
	assertJudgeDirectAssignment(t, ctx, pool, 707, "judge.submission.view.own", "allow", true)
	assertJudgeDirectAssignment(t, ctx, pool, 708, "judge.submission.view.own", "deny", true)
	assertJudgeDirectAssignment(t, ctx, pool, 709, "judge.submission.view.own", "allow", true)
	assertJudgeDirectAssignment(t, ctx, pool, 710, "judge.submission.view.own", "allow", true)
	assertJudgeAssignmentDoesNotExpire(t, ctx, pool, 709, "judge.submission.view.own")
	assertJudgeAssignmentDoesNotExpire(t, ctx, pool, 710, "judge.submission.view.own")
	assertJudgeDirectAssignment(t, ctx, pool, 999, "judge.submission.view.own", "allow", true)
	assertJudgeEffectivePermission(t, ctx, pool, 703, "judge.submission.view.own", false)
	assertJudgeEffectivePermission(t, ctx, pool, 704, "judge.submission.manage", false)
	assertJudgeEffectivePermission(t, ctx, pool, 705, "judge.submission.manage", false)
	assertJudgeEffectivePermission(t, ctx, pool, 707, "judge.submission.view.own", true)
	assertJudgeEffectivePermission(t, ctx, pool, 708, "judge.submission.view.own", false)
	assertJudgeEffectivePermission(t, ctx, pool, 709, "judge.submission.view.own", true)
	assertJudgeEffectivePermission(t, ctx, pool, 710, "judge.submission.view.own", true)
	assertJudgeEffectivePermission(t, ctx, pool, 999, "judge.submission.view.own", true)
	assertJudgeEffectivePermission(t, ctx, pool, 800, "judge.submission.view.own", true)
	assertJudgeEffectivePermission(t, ctx, pool, 800, "judge.submission.view.all", false)
	assertJudgeEffectivePermission(t, ctx, pool, 800, "judge.submission.manage", false)
	assertJudgeEffectivePermission(t, ctx, pool, 801, "judge.submission.view.own", true)
	assertJudgeEffectivePermission(t, ctx, pool, 801, "judge.submission.view.all", true)
	assertJudgeEffectivePermission(t, ctx, pool, 801, "judge.submission.manage", true)

	var legacyPermissionCount int
	if err := pool.QueryRow(ctx, `
SELECT COUNT(*) FROM permissions
WHERE code IN ('submission.view.own', 'submission.view.all', 'submission.rejudge', 'submission.delete')
`).Scan(&legacyPermissionCount); err != nil {
		t.Fatal(err)
	}
	if legacyPermissionCount != 4 {
		t.Fatalf("legacy permissions were not retained: %d", legacyPermissionCount)
	}

	// Reapply and rollback must both preserve the exact expand-only state,
	// including pre-existing Contribution definitions and administrator grants.
	applyJudgePermissionMigration(t, ctx, pool, "000016_judge_permission_namespace.up.sql")
	before := judgePermissionState(t, ctx, pool)
	applyJudgePermissionMigration(t, ctx, pool, "000016_judge_permission_namespace.down.sql")
	afterRollback := judgePermissionState(t, ctx, pool)
	if before != afterRollback {
		t.Fatalf("expand-only rollback changed authorization state: before=%v after=%v", before, afterRollback)
	}
	applyJudgePermissionMigration(t, ctx, pool, "000016_judge_permission_namespace.up.sql")
	if afterReapply := judgePermissionState(t, ctx, pool); afterReapply != before {
		t.Fatalf("migration reapply is not idempotent: before=%v after=%v", before, afterReapply)
	}
}

func TestJudgePermissionNamespacePostgresRejectsForeignOwnership(t *testing.T) {
	databaseURL := os.Getenv("AUTH_JUDGE_PERMISSION_TEST_DATABASE_URL")
	if databaseURL == "" {
		databaseURL = os.Getenv("AUTH_BOOTSTRAP_TEST_DATABASE_URL")
	}
	if databaseURL == "" {
		t.Skip("AUTH_JUDGE_PERMISSION_TEST_DATABASE_URL is not configured")
	}
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	adminPool, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	defer adminPool.Close()
	schema := fmt.Sprintf("auth_judge_permission_conflict_%d", time.Now().UnixNano())
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
	applyJudgePermissionMigration(t, ctx, pool, "000001_init_schema.up.sql")
	applyJudgePermissionMigration(t, ctx, pool, "000003_permission_core.up.sql")
	if _, err := pool.Exec(ctx, `
INSERT INTO permissions(code, service_code, name, description)
VALUES('judge.submission.view.own', 'foreign-service', 'Foreign', '')
`); err != nil {
		t.Fatal(err)
	}
	contents, err := os.ReadFile(filepath.Join("..", "..", "migrations", "000016_judge_permission_namespace.up.sql"))
	if err != nil {
		t.Fatal(err)
	}
	tx, err := pool.Begin(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := tx.Exec(ctx, string(contents)); err == nil {
		_ = tx.Rollback(ctx)
		t.Fatal("migration accepted a foreign owner for the Judge permission namespace")
	}
	if err := tx.Rollback(ctx); err != nil && !errors.Is(err, pgx.ErrTxClosed) {
		t.Fatal(err)
	}
	var serviceCode string
	if err := pool.QueryRow(ctx, `SELECT service_code FROM permissions WHERE code='judge.submission.view.own'`).Scan(&serviceCode); err != nil {
		t.Fatal(err)
	}
	if serviceCode != "foreign-service" {
		t.Fatalf("migration rewrote foreign ownership: %s", serviceCode)
	}
	var partialInsertCount int
	if err := pool.QueryRow(ctx, `
SELECT COUNT(*) FROM permissions
WHERE code IN ('judge.submission.view.all', 'judge.submission.manage')
`).Scan(&partialInsertCount); err != nil {
		t.Fatal(err)
	}
	if partialInsertCount != 0 {
		t.Fatalf("foreign ownership rejection partially inserted permissions: %d", partialInsertCount)
	}
}

func applyJudgePermissionMigration(t *testing.T, ctx context.Context, pool *pgxpool.Pool, name string) {
	t.Helper()
	contents, err := os.ReadFile(filepath.Join("..", "..", "migrations", name))
	if err != nil {
		t.Fatal(err)
	}
	tx, err := pool.Begin(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	if _, err := tx.Exec(ctx, string(contents)); err != nil {
		t.Fatalf("apply %s: %v", name, err)
	}
	if err := tx.Commit(ctx); err != nil {
		t.Fatalf("commit %s: %v", name, err)
	}
}

func assertJudgeRolePermissions(
	t *testing.T,
	ctx context.Context,
	pool *pgxpool.Pool,
	role string,
	want []string,
) {
	t.Helper()
	rows, err := pool.Query(ctx, `
SELECT rp.permission_code
FROM role_permissions rp
JOIN roles r ON r.id = rp.role_id
WHERE r.name = $1 AND rp.permission_code LIKE 'judge.submission.%'
ORDER BY rp.permission_code
`, role)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	got := make([]string, 0)
	for rows.Next() {
		var permission string
		if err := rows.Scan(&permission); err != nil {
			t.Fatal(err)
		}
		got = append(got, permission)
	}
	if err := rows.Err(); err != nil {
		t.Fatal(err)
	}
	if fmt.Sprint(got) != fmt.Sprint(want) {
		t.Fatalf("unexpected %s current Judge permissions: got=%v want=%v", role, got, want)
	}
}

func assertJudgeDirectAssignment(
	t *testing.T,
	ctx context.Context,
	pool *pgxpool.Pool,
	principalID int64,
	permission string,
	wantEffect string,
	wantPresent bool,
) {
	t.Helper()
	var effect string
	err := pool.QueryRow(ctx, `
SELECT effect FROM permission_assignments
WHERE principal_type = 'user' AND principal_id = $1
  AND permission_code = $2 AND scope_type = 'system' AND scope_id = 0
`, principalID, permission).Scan(&effect)
	if !wantPresent {
		if err == nil {
			t.Fatalf("unexpected direct assignment: user=%d permission=%s effect=%s", principalID, permission, effect)
		}
		if !errors.Is(err, pgx.ErrNoRows) {
			t.Fatalf("query direct assignment: user=%d permission=%s: %v", principalID, permission, err)
		}
		return
	}
	if err != nil {
		t.Fatalf("missing direct assignment: user=%d permission=%s: %v", principalID, permission, err)
	}
	if effect != wantEffect {
		t.Fatalf("unexpected direct assignment effect: user=%d permission=%s got=%s want=%s", principalID, permission, effect, wantEffect)
	}
}

func assertJudgeAssignmentDoesNotExpire(
	t *testing.T,
	ctx context.Context,
	pool *pgxpool.Pool,
	principalID int64,
	permissionCode string,
) {
	t.Helper()
	var doesNotExpire bool
	if err := pool.QueryRow(ctx, `
SELECT expires_at IS NULL FROM permission_assignments
WHERE principal_type = 'user' AND principal_id = $1
  AND permission_code = $2 AND scope_type = 'system' AND scope_id = 0
`, principalID, permissionCode).Scan(&doesNotExpire); err != nil {
		t.Fatal(err)
	}
	if !doesNotExpire {
		t.Fatalf("assignment unexpectedly expires: user=%d permission=%s", principalID, permissionCode)
	}
}

func judgePermissionState(t *testing.T, ctx context.Context, pool *pgxpool.Pool) [4]int {
	t.Helper()
	var state [4]int
	queries := []string{
		`SELECT COUNT(*) FROM permissions WHERE code LIKE 'judge.submission.%'`,
		`SELECT COUNT(*) FROM role_permissions WHERE permission_code LIKE 'judge.submission.%'`,
		`SELECT COUNT(*) FROM permission_assignments WHERE permission_code LIKE 'judge.submission.%'`,
		`SELECT COUNT(*) FROM contribution_permission_definitions WHERE permission_code LIKE 'judge.submission.%'`,
	}
	for index, query := range queries {
		if err := pool.QueryRow(ctx, query).Scan(&state[index]); err != nil {
			t.Fatal(err)
		}
	}
	return state
}

func assertJudgeEffectivePermission(
	t *testing.T,
	ctx context.Context,
	pool *pgxpool.Pool,
	userID int64,
	permissionCode string,
	want bool,
) {
	t.Helper()
	got, err := sharedperm.HasUserPermission(
		ctx,
		pool,
		userID,
		permissionCode,
		sharedperm.SystemScope(),
	)
	if err != nil {
		t.Fatal(err)
	}
	if got != want {
		t.Fatalf("unexpected effective permission: user=%d permission=%s got=%v want=%v", userID, permissionCode, got, want)
	}
}
