package artifactgc

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

func TestRealPostgresOperatorMigrationPreservesAppendOnlyV1Audit(t *testing.T) {
	db, ctx, cleanup := newOperatorPostgresFixture(t, "audit")
	defer cleanup()
	createPreMigrationArtifactGCSchema(t, ctx, db)
	applyArtifactGCMigration(t, ctx, db, "000007_problem_artifact_gc_needs_attention.up.sql")

	uri := operatorFixtureURI("a")
	if _, err := db.Exec(ctx, `
INSERT INTO problem_artifact_upload_intents(
    artifact_uri, artifact_sha256, artifact_size_bytes, status,
    retry_after, failure_count, last_error, needs_attention_at,
    created_at, updated_at
)
VALUES($1,$2,17,'NEEDS_ATTENTION',NOW(),3,'legacy failure',NOW(),NOW(),NOW())
`, uri, strings.Repeat("a", 64)); err != nil {
		t.Fatal(err)
	}
	var actionID int64
	if err := db.QueryRow(ctx, `
INSERT INTO problem_artifact_gc_operator_actions(
    artifact_uri, action, actor, reason, previous_status,
    previous_failure_count, previous_last_error, previous_needs_attention_at
)
VALUES($1, 'RETRY', 'user:7', 'legacy audit', 'NEEDS_ATTENTION', 3, 'legacy failure', NOW())
RETURNING action_id
`, uri).Scan(&actionID); err != nil {
		t.Fatal(err)
	}
	assertOperatorAuditAppendOnly(t, ctx, db, actionID)

	applyArtifactGCMigration(t, ctx, db, "000008_problem_artifact_gc_operator_api.up.sql")
	var schemaVersion int
	var idempotencyKey *string
	var actor, reason string
	if err := db.QueryRow(ctx, `
SELECT action_schema_version, idempotency_key, actor, reason
FROM problem_artifact_gc_operator_actions WHERE action_id=$1
`, actionID).Scan(&schemaVersion, &idempotencyKey, &actor, &reason); err != nil {
		t.Fatal(err)
	}
	if schemaVersion != 1 || idempotencyKey != nil || actor != "user:7" || reason != "legacy audit" {
		t.Fatalf("v8 migration rewrote v7 audit: version=%d key=%v actor=%q reason=%q", schemaVersion, idempotencyKey, actor, reason)
	}
	assertOperatorAuditAppendOnly(t, ctx, db, actionID)

	applyArtifactGCMigration(t, ctx, db, "000008_problem_artifact_gc_operator_api.down.sql")
	var columnExists bool
	if err := db.QueryRow(ctx, `
SELECT EXISTS (
    SELECT 1 FROM information_schema.columns
    WHERE table_schema=current_schema()
      AND table_name='problem_artifact_gc_operator_actions'
      AND column_name='action_schema_version'
)
`).Scan(&columnExists); err != nil {
		t.Fatal(err)
	}
	if columnExists {
		t.Fatal("successful v8 down left v8 columns behind")
	}
	assertOperatorAuditAppendOnly(t, ctx, db, actionID)
}

func TestRealPostgresOperatorMigrationDownFailsClosedWithV2Audit(t *testing.T) {
	db, ctx, cleanup := newOperatorPostgresFixture(t, "down_guard")
	defer cleanup()
	createPreMigrationArtifactGCSchema(t, ctx, db)
	applyArtifactGCMigration(t, ctx, db, "000007_problem_artifact_gc_needs_attention.up.sql")
	applyArtifactGCMigration(t, ctx, db, "000008_problem_artifact_gc_operator_api.up.sql")

	uri := operatorFixtureURI("b")
	insertOperatorIntent(t, ctx, db, uri, "PENDING", true, 0)
	ledger := PostgresLedger{DB: db}
	action, err := ledger.RequestReconcile(ctx, uri, strings.Repeat("b", 64), 17, "user:8", "verify orphan", "down-guard")
	if err != nil {
		t.Fatal(err)
	}
	downSQL, err := os.ReadFile("../../migrations/000008_problem_artifact_gc_operator_api.down.sql")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(ctx, string(downSQL)); err == nil || !strings.Contains(err.Error(), "v2 audit rows exist") {
		t.Fatalf("v8 down did not fail closed: %v", err)
	}
	var schemaVersion int
	if err := db.QueryRow(ctx, `SELECT action_schema_version FROM problem_artifact_gc_operator_actions WHERE action_id=$1`, action.ActionID).Scan(&schemaVersion); err != nil || schemaVersion != 2 {
		t.Fatalf("failed down damaged v2 schema/audit: version=%d err=%v", schemaVersion, err)
	}
	assertOperatorAuditAppendOnly(t, ctx, db, action.ActionID)
}

func TestRealPostgresOperatorV2AuditConstraintsRejectInvalidSnapshots(t *testing.T) {
	db, ctx, cleanup := newOperatorPostgresFixture(t, "constraints")
	defer cleanup()
	createPreMigrationArtifactGCSchema(t, ctx, db)
	applyArtifactGCMigration(t, ctx, db, "000007_problem_artifact_gc_needs_attention.up.sql")
	applyArtifactGCMigration(t, ctx, db, "000008_problem_artifact_gc_operator_api.up.sql")

	attention := time.Now().UTC()
	tests := []struct {
		name                string
		action              string
		previousStatus      string
		previousFailures    int
		previousAttention   any
		fromStatus          string
		previousFailureKind string
		constraint          string
	}{
		{name: "retry requires attention snapshot", action: "RETRY", previousStatus: "NEEDS_ATTENTION", previousFailures: 3, previousAttention: nil, fromStatus: "NEEDS_ATTENTION", previousFailureKind: FailureKindTransient, constraint: "chk_problem_artifact_gc_action_attention_snapshot_v2"},
		{name: "retry requires a positive failure count", action: "RETRY", previousStatus: "NEEDS_ATTENTION", previousFailures: 0, previousAttention: attention, fromStatus: "NEEDS_ATTENTION", previousFailureKind: FailureKindTransient, constraint: "chk_problem_artifact_gc_action_previous_failures_v2"},
		{name: "from status must equal snapshot", action: "RETRY", previousStatus: "NEEDS_ATTENTION", previousFailures: 3, previousAttention: attention, fromStatus: "PENDING", previousFailureKind: FailureKindTransient, constraint: "chk_problem_artifact_gc_action_transition_v2"},
		{name: "failure kind is closed", action: "RETRY", previousStatus: "NEEDS_ATTENTION", previousFailures: 3, previousAttention: attention, fromStatus: "NEEDS_ATTENTION", previousFailureKind: "UNKNOWN", constraint: "chk_problem_artifact_gc_action_failure_kind_v2"},
	}
	for index, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := db.Exec(ctx, `
INSERT INTO problem_artifact_gc_operator_actions(
    action_schema_version, artifact_uri, action, actor, reason,
    previous_status, previous_failure_count, previous_last_error,
    previous_needs_attention_at, idempotency_key, request_hash,
    artifact_sha256, artifact_size_bytes, from_status, to_status,
    previous_last_failure_kind
)
VALUES(2,$1,$2,'user:1','verified',$3,$4,'failure',$5,$6,$7,$8,17,$9,'PENDING',$10)
`, operatorFixtureURI("e"), tt.action, tt.previousStatus, tt.previousFailures,
				tt.previousAttention, fmt.Sprintf("invalid-snapshot-%d", index), strings.Repeat("a", 64), strings.Repeat("e", 64), tt.fromStatus, tt.previousFailureKind)
			if err == nil || !strings.Contains(err.Error(), tt.constraint) {
				t.Fatalf("invalid v2 snapshot was not rejected by %s: %v", tt.constraint, err)
			}
		})
	}

	// RECONCILE snapshots a healthy PENDING row, so a nil attention timestamp
	// and zero failures are both intentional and must remain valid.
	if _, err := db.Exec(ctx, `
INSERT INTO problem_artifact_gc_operator_actions(
    action_schema_version, artifact_uri, action, actor, reason,
    previous_status, previous_failure_count, previous_last_error,
    previous_needs_attention_at, idempotency_key, request_hash,
    artifact_sha256, artifact_size_bytes, from_status, to_status
)
VALUES(2,$1,'RECONCILE','user:1','verified','PENDING',0,'',NULL,$2,$3,$4,17,'PENDING','PENDING')
`, operatorFixtureURI("f"), "valid-reconcile", strings.Repeat("b", 64), strings.Repeat("f", 64)); err != nil {
		t.Fatalf("valid reconcile snapshot was rejected: %v", err)
	}
}

func TestRealPostgresLegacyNullUploadRetryIsRestartRecoverable(t *testing.T) {
	db, ctx, cleanup := newOperatorPostgresFixture(t, "legacy_retry_recovery")
	defer cleanup()
	createPreMigrationArtifactGCSchema(t, ctx, db)
	applyArtifactGCMigration(t, ctx, db, "000007_problem_artifact_gc_needs_attention.up.sql")
	applyArtifactGCMigration(t, ctx, db, "000008_problem_artifact_gc_operator_api.up.sql")
	ledger := PostgresLedger{DB: db}
	uri := operatorFixtureURI("9")
	insertOperatorIntent(t, ctx, db, uri, "NEEDS_ATTENTION", false, 3)

	if _, err := ledger.RetryNeedsAttention(ctx, uri, 3, "user:11", "legacy upload verified", "legacy-null-retry"); err != nil {
		t.Fatal(err)
	}
	var status string
	var manualAt, uploadedAt *time.Time
	if err := db.QueryRow(ctx, `
SELECT status, manual_reconcile_requested_at, upload_completed_at
FROM problem_artifact_upload_intents WHERE artifact_uri=$1
`, uri).Scan(&status, &manualAt, &uploadedAt); err != nil {
		t.Fatal(err)
	}
	if status != "PENDING" || manualAt != nil || uploadedAt != nil {
		t.Fatalf("legacy retry fabricated upload completion: status=%s manual=%v uploaded=%v", status, manualAt, uploadedAt)
	}
	if due, err := ledger.RecoveryDue(ctx); err != nil || !due {
		t.Fatalf("durable retry marker was not restart-recoverable: due=%v err=%v", due, err)
	}
	claimed, err := ledger.Claim(ctx, time.Now().Add(-365*24*time.Hour), time.Minute)
	if err != nil || claimed == nil || claimed.URI != uri {
		t.Fatalf("restart recovery did not target the legacy retry: claim=%#v err=%v", claimed, err)
	}
}

func TestRealPostgresOperatorActionsAreIdempotentAndCursorBounded(t *testing.T) {
	db, ctx, cleanup := newOperatorPostgresFixture(t, "actions")
	defer cleanup()
	createPreMigrationArtifactGCSchema(t, ctx, db)
	applyArtifactGCMigration(t, ctx, db, "000007_problem_artifact_gc_needs_attention.up.sql")
	applyArtifactGCMigration(t, ctx, db, "000008_problem_artifact_gc_operator_api.up.sql")
	ledger := PostgresLedger{DB: db}

	reconcileURI := operatorFixtureURI("c")
	insertOperatorIntent(t, ctx, db, reconcileURI, "PENDING", false, 0)
	if _, err := ledger.RequestReconcile(ctx, reconcileURI, strings.Repeat("c", 64), 17, "user:9", "verify", "before-upload"); !errors.Is(err, ErrOperatorStateConflict) {
		t.Fatalf("unverified upload was reconciled: %v", err)
	}
	if _, err := db.Exec(ctx, `UPDATE problem_artifact_upload_intents SET upload_completed_at=NOW() WHERE artifact_uri=$1`, reconcileURI); err != nil {
		t.Fatal(err)
	}

	results := make([]OperatorActionResult, 2)
	errs := make([]error, 2)
	start := make(chan struct{})
	var wait sync.WaitGroup
	for index := range results {
		wait.Add(1)
		go func(index int) {
			defer wait.Done()
			<-start
			results[index], errs[index] = ledger.RequestReconcile(ctx, reconcileURI, strings.Repeat("c", 64), 17, "user:9", "verify", "same-reconcile")
		}(index)
	}
	close(start)
	wait.Wait()
	if errs[0] != nil || errs[1] != nil || results[0].ActionID <= 0 || results[0].ActionID != results[1].ActionID || results[0].Replayed == results[1].Replayed {
		t.Fatalf("concurrent reconcile was not one action plus one replay: results=%#v errs=%v", results, errs)
	}
	if _, err := ledger.RequestReconcile(ctx, reconcileURI, strings.Repeat("c", 64), 17, "user:9", "different", "same-reconcile"); !errors.Is(err, ErrOperatorIdempotencyConflict) {
		t.Fatalf("same key with a different payload was not rejected: %v", err)
	}

	retryURI := operatorFixtureURI("d")
	insertOperatorIntent(t, ctx, db, retryURI, "NEEDS_ATTENTION", true, 3)
	results = make([]OperatorActionResult, 2)
	errs = make([]error, 2)
	locker, err := db.Acquire(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer locker.Release()
	workers := make([]*pgxpool.Conn, 2)
	for index := range workers {
		workers[index], err = db.Acquire(ctx)
		if err != nil {
			t.Fatal(err)
		}
		defer workers[index].Release()
		if _, err := workers[index].Exec(ctx, `SELECT set_config('application_name', $1, false)`, fmt.Sprintf("ojos_gc_retry_waiter_%d", index)); err != nil {
			t.Fatal(err)
		}
	}
	lockTx, err := locker.Begin(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer lockTx.Rollback(context.Background())
	if _, err := lockTx.Exec(ctx, `SELECT artifact_uri FROM problem_artifact_upload_intents WHERE artifact_uri=$1 FOR UPDATE`, retryURI); err != nil {
		t.Fatal(err)
	}
	start = make(chan struct{})
	for index := range results {
		wait.Add(1)
		go func(index int) {
			defer wait.Done()
			<-start
			results[index], errs[index] = (PostgresLedger{DB: workers[index]}).RetryNeedsAttention(ctx, retryURI, 3, "user:9", "retry approved", "same-retry")
		}(index)
	}
	close(start)
	deadline := time.Now().Add(5 * time.Second)
	for {
		var blocked int
		if err := db.QueryRow(ctx, `
SELECT COUNT(*)
FROM pg_stat_activity
WHERE application_name IN ('ojos_gc_retry_waiter_0', 'ojos_gc_retry_waiter_1')
  AND state = 'active'
  AND wait_event_type = 'Lock'
  AND query LIKE '%problem_artifact_upload_intents%'
`).Scan(&blocked); err != nil {
			t.Fatal(err)
		}
		if blocked == 2 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("both retry requests did not reach the intent row lock after their idempotency prelookup: blocked=%d", blocked)
		}
		time.Sleep(10 * time.Millisecond)
	}
	if err := lockTx.Commit(ctx); err != nil {
		t.Fatal(err)
	}
	wait.Wait()
	if errs[0] != nil || errs[1] != nil || results[0].ActionID <= 0 || results[0].ActionID != results[1].ActionID || results[0].Replayed == results[1].Replayed {
		t.Fatalf("concurrent retry was not one action plus one replay: results=%#v errs=%v", results, errs)
	}

	page, err := ledger.ListIntents(ctx, "PENDING", "", 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(page.Items) != 1 || page.NextCursor == "" {
		t.Fatalf("bounded page did not return a stable cursor: %#v", page)
	}
	next, err := ledger.ListIntents(ctx, "PENDING", page.NextCursor, 1)
	if err != nil || len(next.Items) == 0 || next.Items[0].URI <= page.NextCursor {
		t.Fatalf("cursor did not advance strictly by URI: next=%#v err=%v", next, err)
	}
}

func newOperatorPostgresFixture(t *testing.T, suffix string) (*pgxpool.Pool, context.Context, func()) {
	t.Helper()
	databaseURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if databaseURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	admin, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		cancel()
		t.Fatal(err)
	}
	schema := fmt.Sprintf("ojos_artifact_gc_operator_%s_%d", suffix, time.Now().UTC().UnixNano())
	if _, err := admin.Exec(ctx, "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
		admin.Close()
		cancel()
		t.Fatal(err)
	}
	db := gcPoolWithSearchPath(t, ctx, databaseURL, schema)
	return db, ctx, func() {
		db.Close()
		_, _ = admin.Exec(context.Background(), "DROP SCHEMA IF EXISTS "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
		admin.Close()
		cancel()
	}
}

func operatorFixtureURI(character string) string {
	digest := strings.Repeat(character, 64)
	return "storage://problems/package-sha256-" + digest + ".zip"
}

func insertOperatorIntent(t *testing.T, ctx context.Context, db *pgxpool.Pool, uri, status string, uploaded bool, failures int) {
	t.Helper()
	digest := strings.TrimSuffix(strings.TrimPrefix(uri, "storage://problems/package-sha256-"), ".zip")
	var uploadedAt any
	if uploaded {
		uploadedAt = time.Now().UTC()
	}
	attentionAt := any(nil)
	lastError := ""
	lastKind := ""
	if status == "NEEDS_ATTENTION" {
		attentionAt = time.Now().UTC()
		lastError = "provider request failed"
		lastKind = FailureKindTransient
	}
	if _, err := db.Exec(ctx, `
INSERT INTO problem_artifact_upload_intents(
    artifact_uri, artifact_sha256, artifact_size_bytes, status,
    retry_after, failure_count, last_error, needs_attention_at,
    upload_completed_at, last_failure_stage, last_failure_kind,
    created_at, updated_at
)
VALUES($1,$2,17,$3,NOW(),$4,$5,$6,$7,'inspect',$8,NOW(),NOW())
`, uri, digest, status, failures, lastError, attentionAt, uploadedAt, lastKind); err != nil {
		t.Fatal(err)
	}
}

func assertOperatorAuditAppendOnly(t *testing.T, ctx context.Context, db *pgxpool.Pool, actionID int64) {
	t.Helper()
	for name, statement := range map[string]string{
		"update":   `UPDATE problem_artifact_gc_operator_actions SET reason='tampered' WHERE action_id=$1`,
		"delete":   `DELETE FROM problem_artifact_gc_operator_actions WHERE action_id=$1`,
		"truncate": `TRUNCATE problem_artifact_gc_operator_actions`,
	} {
		var err error
		if name == "truncate" {
			_, err = db.Exec(ctx, statement)
		} else {
			_, err = db.Exec(ctx, statement, actionID)
		}
		if err == nil || !strings.Contains(err.Error(), "append-only") {
			t.Fatalf("operator audit allowed %s: %v", name, err)
		}
	}
}
