package artifactgc

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

func TestRealPostgresNeedsAttentionMigrationAndOperatorRecovery(t *testing.T) {
	databaseURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if databaseURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	admin, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	defer admin.Close()
	schema := fmt.Sprintf("ojos_artifact_gc_attention_%d", time.Now().UTC().UnixNano())
	if _, err := admin.Exec(ctx, "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
		t.Fatal(err)
	}
	defer admin.Exec(context.Background(), "DROP SCHEMA IF EXISTS "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
	db := gcPoolWithSearchPath(t, ctx, databaseURL, schema)
	defer db.Close()
	createPreMigrationArtifactGCSchema(t, ctx, db)
	applyArtifactGCMigration(t, ctx, db, "000007_problem_artifact_gc_needs_attention.up.sql")
	applyArtifactGCMigration(t, ctx, db, "000008_problem_artifact_gc_operator_api.up.sql")

	digest := strings.Repeat("a", 64)
	uri := "storage://problems/package-sha256-" + digest + ".zip"
	old := time.Now().Add(-8 * 24 * time.Hour)
	if _, err := db.Exec(ctx, `
INSERT INTO problem_artifact_upload_intents(
    artifact_uri, artifact_sha256, artifact_size_bytes, status,
    retry_after, created_at, updated_at
) VALUES($1,$2,17,'PENDING',$3,$3,$3)
`, uri, digest, old); err != nil {
		t.Fatal(err)
	}

	ledger := PostgresLedger{DB: db}
	first := claimAttentionFixture(t, ctx, db, ledger, time.Now().Add(-7*24*time.Hour))
	if first.FailureCount != 0 {
		t.Fatalf("new intent inherited failures: %#v", first)
	}
	if err := ledger.Retry(ctx, *first, FailureDetail{Message: "inspect failed transiently", Stage: "inspect", Kind: FailureKindTransient}, 0); err != nil {
		t.Fatal(err)
	}
	expireAttentionClaim(t, ctx, db, uri)
	second := claimAttentionFixture(t, ctx, db, ledger, time.Now().Add(-7*24*time.Hour))
	if second.ClaimToken == first.ClaimToken || second.FailureCount != 1 {
		t.Fatalf("second failure did not use the persisted failure count and new token: first=%#v second=%#v", first, second)
	}
	if err := ledger.Retry(ctx, *second, FailureDetail{Message: "inspect failed transiently", Stage: "inspect", Kind: FailureKindTransient}, 0); err != nil {
		t.Fatal(err)
	}
	expireAttentionClaim(t, ctx, db, uri)

	report, runErr := (Collector{
		Ledger:    ledger,
		Store:     &fakeStore{inspectErr: errors.New("dial tcp: connection refused")},
		Delete:    true,
		BatchSize: 1,
	}).Run(ctx)
	if runErr == nil || len(report.NeedsAttention) != 1 || report.NeedsAttention[0] != uri {
		t.Fatalf("third transient failure did not terminate: err=%v report=%#v", runErr, report)
	}

	var status, lastError string
	var claimToken *string
	var failureCount int
	var needsAttentionAt time.Time
	if err := db.QueryRow(ctx, `
SELECT status, claim_token, failure_count, last_error, needs_attention_at
FROM problem_artifact_upload_intents WHERE artifact_uri=$1
`, uri).Scan(&status, &claimToken, &failureCount, &lastError, &needsAttentionAt); err != nil {
		t.Fatal(err)
	}
	if status != "NEEDS_ATTENTION" || claimToken != nil || failureCount != MaxAutomaticFailures ||
		lastError == "" || needsAttentionAt.IsZero() {
		t.Fatalf("invalid terminal state: status=%s token=%v failures=%d error=%q at=%s", status, claimToken, failureCount, lastError, needsAttentionAt)
	}
	if claim, err := ledger.Claim(ctx, time.Now(), time.Minute); err != nil || claim != nil {
		t.Fatalf("NEEDS_ATTENTION was claimable: claim=%#v err=%v", claim, err)
	}
	if err := ledger.Quarantine(ctx, *second, FailureDetail{Message: "stale collector", Stage: "test", Kind: FailureKindTransient}); !errors.Is(err, ErrClaimLost) {
		t.Fatalf("stale claim quarantined terminal row: %v", err)
	}
	if _, err := ledger.RetryNeedsAttention(ctx, uri, MaxAutomaticFailures, "", "verified", "empty-actor"); !errors.Is(err, ErrOperatorActorMissing) {
		t.Fatalf("empty operator actor was accepted: %v", err)
	}
	if _, err := ledger.RetryNeedsAttention(ctx, uri, MaxAutomaticFailures, "admin:test", "", "empty-reason"); !errors.Is(err, ErrOperatorRetryReasonMissing) {
		t.Fatalf("empty operator reason was accepted: %v", err)
	}

	if _, err := db.Exec(ctx, `
CREATE FUNCTION fail_problem_artifact_gc_audit_insert()
RETURNS trigger LANGUAGE plpgsql AS $ojos$
BEGIN
    RAISE EXCEPTION 'injected operator audit failure';
END
$ojos$;
CREATE TRIGGER trg_fail_problem_artifact_gc_audit_insert
BEFORE INSERT ON problem_artifact_gc_operator_actions
FOR EACH ROW EXECUTE FUNCTION fail_problem_artifact_gc_audit_insert();
`); err != nil {
		t.Fatal(err)
	}
	if _, err := ledger.RetryNeedsAttention(ctx, uri, MaxAutomaticFailures, "admin:test", "first recovery must roll back", "audit-failure"); err == nil || !strings.Contains(err.Error(), "injected operator audit failure") {
		t.Fatalf("injected audit failure did not abort operator recovery: %v", err)
	}
	if err := db.QueryRow(ctx, `SELECT status FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, uri).Scan(&status); err != nil || status != "NEEDS_ATTENTION" {
		t.Fatalf("operator state update escaped failed audit transaction: status=%s err=%v", status, err)
	}
	if _, err := db.Exec(ctx, `
DROP TRIGGER trg_fail_problem_artifact_gc_audit_insert ON problem_artifact_gc_operator_actions;
DROP FUNCTION fail_problem_artifact_gc_audit_insert();
`); err != nil {
		t.Fatal(err)
	}

	reason := "provider identity verified; retry approved"
	action, err := ledger.RetryNeedsAttention(ctx, uri, MaxAutomaticFailures, "admin:test", reason, "retry-success")
	if err != nil || action.ActionID <= 0 {
		t.Fatalf("operator retry failed: action=%#v err=%v", action, err)
	}
	actionID := action.ActionID
	var actor, auditReason, previousStatus, previousError string
	var previousFailures int
	var previousAttentionAt time.Time
	if err := db.QueryRow(ctx, `
SELECT actor, reason, previous_status, previous_failure_count,
       previous_last_error, previous_needs_attention_at
FROM problem_artifact_gc_operator_actions WHERE action_id=$1
`, actionID).Scan(&actor, &auditReason, &previousStatus, &previousFailures, &previousError, &previousAttentionAt); err != nil {
		t.Fatal(err)
	}
	if actor != "admin:test" || auditReason != reason || previousStatus != "NEEDS_ATTENTION" ||
		previousFailures != MaxAutomaticFailures || previousError != lastError || !previousAttentionAt.Equal(needsAttentionAt) {
		t.Fatalf("operator audit snapshot mismatch: actor=%s reason=%s status=%s failures=%d error=%q at=%s", actor, auditReason, previousStatus, previousFailures, previousError, previousAttentionAt)
	}
	var operatorReason string
	if err := db.QueryRow(ctx, `
SELECT status, claim_token, failure_count, last_error, needs_attention_at,
       last_operator_retry_reason
FROM problem_artifact_upload_intents WHERE artifact_uri=$1
`, uri).Scan(&status, &claimToken, &failureCount, &lastError, &needsAttentionAt, &operatorReason); err != nil {
		t.Fatal(err)
	}
	if status != "PENDING" || claimToken != nil || failureCount != 0 || operatorReason != reason ||
		lastError == "" || needsAttentionAt.IsZero() {
		t.Fatalf("operator recovery did not preserve forensic state: status=%s token=%v failures=%d error=%q at=%s reason=%q", status, claimToken, failureCount, lastError, needsAttentionAt, operatorReason)
	}
	if _, err := db.Exec(ctx, `UPDATE problem_artifact_gc_operator_actions SET reason='tampered' WHERE action_id=$1`, actionID); err == nil || !strings.Contains(err.Error(), "append-only") {
		t.Fatalf("operator audit allowed mutation: %v", err)
	}
	if _, err := db.Exec(ctx, `DELETE FROM problem_artifact_gc_operator_actions WHERE action_id=$1`, actionID); err == nil || !strings.Contains(err.Error(), "append-only") {
		t.Fatalf("operator audit allowed deletion: %v", err)
	}
	if _, err := db.Exec(ctx, `TRUNCATE problem_artifact_gc_operator_actions`); err == nil || !strings.Contains(err.Error(), "append-only") {
		t.Fatalf("operator audit allowed truncation: %v", err)
	}

	recovered := claimAttentionFixture(t, ctx, db, ledger, time.Now().Add(-7*24*time.Hour))
	if recovered.ClaimToken == first.ClaimToken || recovered.ClaimToken == second.ClaimToken || recovered.FailureCount != 0 {
		t.Fatalf("operator retry did not create a fresh claim: first=%s second=%s recovered=%#v", first.ClaimToken, second.ClaimToken, recovered)
	}
}

func createPreMigrationArtifactGCSchema(t *testing.T, ctx context.Context, db *pgxpool.Pool) {
	t.Helper()
	_, err := db.Exec(ctx, `
CREATE TABLE problems(
    package_artifact_uri TEXT NOT NULL DEFAULT '',
    package_artifact_sha256 TEXT NOT NULL DEFAULT '',
    package_artifact_size_bytes BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE problem_package_revisions(
    artifact_uri TEXT NOT NULL,
    artifact_sha256 TEXT NOT NULL,
    artifact_size_bytes BIGINT NOT NULL
);
CREATE TABLE problem_files(
    storage_path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size_bytes BIGINT NOT NULL
);
CREATE TABLE problem_artifact_upload_intents (
    artifact_uri TEXT PRIMARY KEY,
    artifact_sha256 TEXT NOT NULL,
    artifact_size_bytes BIGINT NOT NULL,
    status TEXT NOT NULL DEFAULT 'PENDING',
    retry_after TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    claim_token TEXT,
    claim_until TIMESTAMPTZ,
    attempt_count INT NOT NULL DEFAULT 0,
    last_error TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_problem_artifact_intent_status CHECK (status IN ('PENDING', 'DELETING')),
    CONSTRAINT chk_problem_artifact_intent_claim CHECK (
        (status = 'PENDING' AND claim_token IS NULL AND claim_until IS NULL)
        OR
        (status = 'DELETING' AND claim_token IS NOT NULL AND claim_until IS NOT NULL)
    )
);
`)
	if err != nil {
		t.Fatal(err)
	}
}

func applyArtifactGCMigration(t *testing.T, ctx context.Context, db *pgxpool.Pool, name string) {
	t.Helper()
	path := filepath.Join("..", "..", "migrations", name)
	sql, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(ctx, string(sql)); err != nil {
		t.Fatalf("apply %s: %v", name, err)
	}
}

func claimAttentionFixture(t *testing.T, ctx context.Context, db *pgxpool.Pool, ledger PostgresLedger, cutoff time.Time) *Intent {
	t.Helper()
	intent, err := ledger.Claim(ctx, cutoff, 10*time.Minute)
	if err != nil || intent == nil {
		t.Fatalf("claim artifact intent: intent=%#v err=%v", intent, err)
	}
	return intent
}

func expireAttentionClaim(t *testing.T, ctx context.Context, db *pgxpool.Pool, uri string) {
	t.Helper()
	if _, err := db.Exec(ctx, `
UPDATE problem_artifact_upload_intents
SET claim_until=NOW()-INTERVAL '1 second', retry_after=NOW()-INTERVAL '1 second'
WHERE artifact_uri=$1
`, uri); err != nil {
		t.Fatal(err)
	}
}
