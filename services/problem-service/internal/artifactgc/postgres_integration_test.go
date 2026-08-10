package artifactgc

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

func TestRealPostgresLedgerClaimsOnlyUnlinkedProblemOwnedOrphans(t *testing.T) {
	databaseURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if databaseURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	admin, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	defer admin.Close()
	schema := fmt.Sprintf("ojos_artifact_gc_ledger_%d", time.Now().UTC().UnixNano())
	if _, err := admin.Exec(ctx, "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
		t.Fatal(err)
	}
	defer admin.Exec(context.Background(), "DROP SCHEMA IF EXISTS "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
	db := gcPoolWithSearchPath(t, ctx, databaseURL, schema)
	defer db.Close()
	if _, err := db.Exec(ctx, `
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
    status TEXT NOT NULL,
    retry_after TIMESTAMPTZ NOT NULL,
    claim_token TEXT,
    claim_until TIMESTAMPTZ,
    attempt_count INT NOT NULL DEFAULT 0,
    failure_count INT NOT NULL DEFAULT 0,
    last_error TEXT NOT NULL DEFAULT '',
    needs_attention_at TIMESTAMPTZ,
    last_operator_retry_reason TEXT NOT NULL DEFAULT '',
    last_operator_retry_at TIMESTAMPTZ,
    upload_completed_at TIMESTAMPTZ,
    manual_reconcile_requested_at TIMESTAMPTZ,
    last_failure_stage TEXT NOT NULL DEFAULT '',
    last_failure_kind TEXT NOT NULL DEFAULT '',
    last_failure_http_status INT,
    last_failure_provider_result TEXT NOT NULL DEFAULT '',
    last_failure_deterministic BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
`); err != nil {
		t.Fatal(err)
	}
	old := time.Now().Add(-8 * 24 * time.Hour)
	digest := func(ch string) string { return strings.Repeat(ch, 64) }
	uri := func(ch string) string { return "storage://problems/package-sha256-" + digest(ch) + ".zip" }
	for _, ch := range []string{"a", "b", "c"} {
		if _, err := db.Exec(ctx, `
INSERT INTO problem_artifact_upload_intents(
 artifact_uri, artifact_sha256, artifact_size_bytes, status, retry_after, created_at, updated_at
) VALUES($1,$2,17,'PENDING',$3,$3,$3)
`, uri(ch), digest(ch), old); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := db.Exec(ctx, `INSERT INTO problems(package_artifact_uri, package_artifact_sha256, package_artifact_size_bytes) VALUES($1,$2,17)`, uri("a"), digest("a")); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(ctx, `INSERT INTO problem_package_revisions(artifact_uri, artifact_sha256, artifact_size_bytes) VALUES($1,$2,17)`, uri("b"), digest("b")); err != nil {
		t.Fatal(err)
	}
	ledger := PostgresLedger{DB: db}
	claimed, err := ledger.Claim(ctx, time.Now().Add(-7*24*time.Hour), time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	if claimed == nil || claimed.URI != uri("a") {
		t.Fatalf("oldest referenced intent was not claimed for ledger cleanup: %#v", claimed)
	}
	if deletable, err := ledger.ConfirmDeletable(ctx, *claimed); err != nil || deletable {
		t.Fatalf("current Problem reference was considered deletable: deletable=%v err=%v", deletable, err)
	}
	if err := ledger.CompleteReferenced(ctx, *claimed); err != nil {
		t.Fatal(err)
	}
	claimed, err = ledger.Claim(ctx, time.Now().Add(-7*24*time.Hour), time.Minute)
	if err != nil || claimed == nil || claimed.URI != uri("b") {
		t.Fatalf("revision-referenced intent was not claimed for cleanup: claim=%#v err=%v", claimed, err)
	}
	if deletable, err := ledger.ConfirmDeletable(ctx, *claimed); err != nil || deletable {
		t.Fatalf("package revision reference was considered deletable: deletable=%v err=%v", deletable, err)
	}
	if err := ledger.CompleteReferenced(ctx, *claimed); err != nil {
		t.Fatal(err)
	}
	claimed, err = ledger.Claim(ctx, time.Now().Add(-7*24*time.Hour), time.Minute)
	if err != nil || claimed == nil || claimed.URI != uri("c") || claimed.Key != "package-sha256-"+digest("c")+".zip" {
		t.Fatalf("unlinked package orphan was not claimed after referenced ledgers: claim=%#v err=%v", claimed, err)
	}
	if deletable, err := ledger.ConfirmDeletable(ctx, *claimed); err != nil || !deletable {
		t.Fatalf("unlinked package orphan was not deletable: deletable=%v err=%v", deletable, err)
	}
	previousClaimUntil := claimed.ClaimUntil
	if err := ledger.Renew(ctx, *claimed, 10*time.Minute); err != nil {
		t.Fatalf("renew delete isolation: %v", err)
	}
	if err := db.QueryRow(ctx, `SELECT claim_until FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, claimed.URI).Scan(&claimed.ClaimUntil); err != nil || !claimed.ClaimUntil.After(previousClaimUntil) {
		t.Fatalf("claim renewal did not extend isolation: before=%s after=%s err=%v", previousClaimUntil, claimed.ClaimUntil, err)
	}
	if err := ledger.Retry(ctx, *claimed, FailureDetail{Message: "delete timed out", Stage: "conditional delete", Kind: FailureKindTransient}, time.Minute); err != nil {
		t.Fatal(err)
	}
	var status string
	var token *string
	var retryClaimUntil time.Time
	if err := db.QueryRow(ctx, `SELECT status, claim_token, claim_until FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, claimed.URI).Scan(&status, &token, &retryClaimUntil); err != nil {
		t.Fatal(err)
	}
	if status != "DELETING" || token == nil || *token != claimed.ClaimToken {
		t.Fatalf("retry exposed deleting object to publishers: status=%s token=%v", status, token)
	}
	if retryClaimUntil.Before(claimed.ClaimUntil) {
		t.Fatalf("retry shortened delete isolation lease: before=%s after=%s", claimed.ClaimUntil, retryClaimUntil)
	}
	if err := ledger.CompleteReferenced(ctx, *claimed); !errors.Is(err, ErrClaimLost) {
		t.Fatalf("unreferenced claim must not complete through the referenced path, got %v", err)
	}
	if err := ledger.CompleteDeleted(ctx, *claimed); err != nil {
		t.Fatal(err)
	}
	if err := ledger.CompleteDeleted(ctx, *claimed); err == nil || !strings.Contains(err.Error(), "claim") {
		t.Fatalf("duplicate completion must report a lost claim, got %v", err)
	}

	contentURI := func(problemID string, ch string) string {
		return "storage://problems/problem-" + problemID + "-objects-sha256-" + digest(ch)
	}
	referenced := Intent{URI: contentURI("41", "d"), ClaimToken: "referenced-claim"}
	if _, err := db.Exec(ctx, `
INSERT INTO problem_artifact_upload_intents(
 artifact_uri, artifact_sha256, artifact_size_bytes, status, retry_after,
 claim_token, claim_until, created_at, updated_at
) VALUES($1,$2,17,'DELETING',$3,$4,NOW() - INTERVAL '1 second',$3,$3)
`, referenced.URI, digest("d"), old, referenced.ClaimToken); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(ctx, `INSERT INTO problem_files(storage_path, sha256, size_bytes) VALUES($1,$2,17)`, referenced.URI, digest("d")); err != nil {
		t.Fatal(err)
	}
	wrongToken := referenced
	wrongToken.ClaimToken = "wrong-claim"
	if err := ledger.CompleteReferenced(ctx, wrongToken); !errors.Is(err, ErrClaimLost) {
		t.Fatalf("referenced completion must use token CAS, got %v", err)
	}
	if err := ledger.CompleteAbsent(ctx, referenced); !errors.Is(err, ErrClaimLost) {
		t.Fatalf("a missing provider object must not erase a still-referenced ledger: %v", err)
	}
	if err := ledger.CompleteReferenced(ctx, referenced); err != nil {
		t.Fatalf("owned claim with an exact problem_files reference must complete: %v", err)
	}

	// URI-only references prevent deletion, but an identity mismatch must not
	// silently discard the forensic ledger entry as "referenced".
	mismatch := Intent{URI: contentURI("42", "e"), ClaimToken: "mismatch-claim"}
	if _, err := db.Exec(ctx, `
INSERT INTO problem_artifact_upload_intents(
 artifact_uri, artifact_sha256, artifact_size_bytes, status, retry_after,
 claim_token, claim_until, created_at, updated_at
) VALUES($1,$2,17,'DELETING',$3,$4,NOW() + INTERVAL '1 minute',$3,$3)
`, mismatch.URI, digest("e"), old, mismatch.ClaimToken); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(ctx, `INSERT INTO problem_files(storage_path, sha256, size_bytes) VALUES($1,$2,18)`, mismatch.URI, digest("f")); err != nil {
		t.Fatal(err)
	}
	if deletable, err := ledger.ConfirmDeletable(ctx, mismatch); err != nil || deletable {
		t.Fatalf("URI-referenced identity mismatch was considered deletable: deletable=%v err=%v", deletable, err)
	}
	if err := ledger.CompleteReferenced(ctx, mismatch); !errors.Is(err, ErrReferenceIdentityMismatch) {
		t.Fatalf("identity-mismatched reference silently completed its ledger: %v", err)
	}
	if err := db.QueryRow(ctx, `SELECT status FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, mismatch.URI).Scan(&status); err != nil || status != "DELETING" {
		t.Fatalf("identity-mismatched ledger was not retained: status=%s err=%v", status, err)
	}
	if _, err := db.Exec(ctx, `UPDATE problem_artifact_upload_intents SET claim_until=NOW()-INTERVAL '1 second', retry_after=NOW()-INTERVAL '1 second' WHERE artifact_uri=$1`, mismatch.URI); err != nil {
		t.Fatal(err)
	}
	mismatchStore := &fakeStore{
		object: Object{Key: objectKey(mismatch.URI), SHA256: digest("e"), SizeBytes: 17},
		exists: true,
	}
	mismatchReport, mismatchErr := (Collector{Ledger: ledger, Store: mismatchStore, Delete: true}).Run(ctx)
	if mismatchErr == nil || len(mismatchStore.deleted) != 0 || len(mismatchReport.Errors) == 0 || len(mismatchReport.NeedsAttention) != 1 {
		t.Fatalf("identity-mismatched reference did not remain fail closed: err=%v report=%#v store=%#v", mismatchErr, mismatchReport, mismatchStore)
	}
	if err := db.QueryRow(ctx, `SELECT status FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, mismatch.URI).Scan(&status); err != nil || status != "NEEDS_ATTENTION" {
		t.Fatalf("identity-mismatched ledger did not enter NEEDS_ATTENTION: status=%s err=%v", status, err)
	}
	if _, err := db.Exec(ctx, `DELETE FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, mismatch.URI); err != nil {
		t.Fatal(err)
	}

	zeroURI := contentURI("43", "0")
	if _, err := db.Exec(ctx, `
INSERT INTO problem_artifact_upload_intents(
 artifact_uri, artifact_sha256, artifact_size_bytes, status, retry_after, created_at, updated_at
) VALUES($1,$2,0,'PENDING',$3,$3,$3)
`, zeroURI, digest("0"), old); err != nil {
		t.Fatal(err)
	}
	zero, err := ledger.Claim(ctx, time.Now().Add(-7*24*time.Hour), time.Minute)
	if err != nil || zero == nil || zero.URI != zeroURI || zero.SizeBytes != 0 {
		t.Fatalf("zero-byte content orphan was not claimable: claim=%#v err=%v", zero, err)
	}
	if deletable, err := ledger.ConfirmDeletable(ctx, *zero); err != nil || !deletable {
		t.Fatalf("zero-byte content orphan was not deletable: deletable=%v err=%v", deletable, err)
	}
	if err := ledger.CompleteDeleted(ctx, *zero); err != nil {
		t.Fatal(err)
	}
}

func gcPoolWithSearchPath(t *testing.T, ctx context.Context, databaseURL, schema string) *pgxpool.Pool {
	t.Helper()
	cfg, err := pgxpool.ParseConfig(databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.ConnConfig.RuntimeParams == nil {
		cfg.ConnConfig.RuntimeParams = map[string]string{}
	}
	cfg.ConnConfig.RuntimeParams["search_path"] = schema
	pool, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	return pool
}
