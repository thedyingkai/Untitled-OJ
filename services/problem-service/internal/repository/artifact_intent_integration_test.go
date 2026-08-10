package repository

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"ojos-problem-service/internal/artifactgc"
	"ojos-problem-service/internal/config"
	"ojos-problem-service/internal/packagefs"
	problemstorage "ojos-problem-service/internal/storage"
	"ojos-shared/eventing"
	"ojos-shared/storagecontract"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

func TestArtifactIntentRollbackIsCollectedButLinkedRevisionIsNeverDeleted(t *testing.T) {
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
	schema := fmt.Sprintf("ojos_artifact_intent_%d", time.Now().UTC().UnixNano())
	if _, err := admin.Exec(ctx, "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
		t.Fatal(err)
	}
	defer admin.Exec(context.Background(), "DROP SCHEMA IF EXISTS "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
	db := artifactIntentPool(t, ctx, databaseURL, schema)
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
    status TEXT NOT NULL DEFAULT 'PENDING',
    retry_after TIMESTAMPTZ NOT NULL DEFAULT NOW(),
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
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
`); err != nil {
		t.Fatal(err)
	}

	objects := &integrationObjects{values: map[string][]byte{}}
	server := httptest.NewServer(http.HandlerFunc(objects.serve))
	defer server.Close()
	repo := New(db)
	packageRoot := t.TempDir()
	packageFile := filepath.Join(packageRoot, "problem.yaml")
	if err := os.WriteFile(packageFile, []byte("title: orphan\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	storageCfg := config.StorageConfig{ServiceEndpoint: server.URL, Bucket: "problems"}
	synced, err := problemstorage.SyncProblemFiles(ctx, storageCfg, 1, []packagefs.IndexedFile{{
		LogicalPath: "problem.yaml", StoragePath: packageFile, MimeType: "application/yaml",
	}}, repo)
	if err != nil {
		t.Fatal(err)
	}
	orphan, err := problemstorage.PublishPackageArtifactTracked(ctx, storageCfg, 1, packageRoot, repo)
	if err != nil {
		t.Fatal(err)
	}
	injected := errors.New("injected business transaction rollback")
	err = repo.InTransaction(ctx, func(txRepo *Repository) error {
		for _, file := range synced {
			if _, err := txRepo.db.Exec(ctx, `INSERT INTO problem_files(storage_path, sha256, size_bytes) VALUES($1,$2,$3)`, file.StoragePath, file.Sha256, file.SizeBytes); err != nil {
				return err
			}
		}
		if err := txRepo.ResolveProblemFileUploadIntents(ctx, synced); err != nil {
			return err
		}
		if _, err := txRepo.db.Exec(ctx, `INSERT INTO problem_package_revisions(artifact_uri, artifact_sha256, artifact_size_bytes) VALUES($1,$2,$3)`, orphan.URI, orphan.SHA256, orphan.SizeBytes); err != nil {
			return err
		}
		if err := txRepo.ResolveArtifactUploadIntent(ctx, orphan); err != nil {
			return err
		}
		return injected
	})
	if !errors.Is(err, injected) {
		t.Fatalf("expected injected rollback, got %v", err)
	}
	var pendingCount int
	if err := db.QueryRow(ctx, `SELECT COUNT(*) FROM problem_artifact_upload_intents WHERE status='PENDING'`).Scan(&pendingCount); err != nil {
		t.Fatal(err)
	}
	if pendingCount != 2 {
		t.Fatalf("rolled-back file and package uploads did not remain PENDING: %d", pendingCount)
	}
	expectedIntents := map[string]eventing.ArtifactRef{
		orphan.URI: orphan,
		synced[0].StoragePath: {
			URI: synced[0].StoragePath, SHA256: synced[0].Sha256, SizeBytes: synced[0].SizeBytes,
		},
	}
	rows, err := db.Query(ctx, `SELECT artifact_uri, artifact_sha256, artifact_size_bytes, status FROM problem_artifact_upload_intents`)
	if err != nil {
		t.Fatal(err)
	}
	for rows.Next() {
		var uri, digest, status string
		var size int64
		if err := rows.Scan(&uri, &digest, &size, &status); err != nil {
			rows.Close()
			t.Fatal(err)
		}
		expected, ok := expectedIntents[uri]
		if !ok || digest != expected.SHA256 || size != expected.SizeBytes || status != "PENDING" {
			rows.Close()
			t.Fatalf("rolled-back upload intent identity mismatch: uri=%s digest=%s size=%d status=%s", uri, digest, size, status)
		}
		delete(expectedIntents, uri)
	}
	if err := rows.Err(); err != nil {
		rows.Close()
		t.Fatal(err)
	}
	rows.Close()
	if len(expectedIntents) != 0 {
		t.Fatalf("rolled-back uploads are missing intents: %#v", expectedIntents)
	}
	if _, err := db.Exec(ctx, `UPDATE problem_artifact_upload_intents SET updated_at=$1, retry_after=$1`, time.Now().Add(-8*24*time.Hour)); err != nil {
		t.Fatal(err)
	}

	tokenPath := filepath.Join(t.TempDir(), "token")
	contextPath := filepath.Join(filepath.Dir(tokenPath), "context.json")
	if err := os.WriteFile(tokenPath, []byte("deployment-workload-token"), 0o600); err != nil {
		t.Fatal(err)
	}
	contextDocument := map[string]any{
		"schema_version": 1,
		"deployment":     map[string]any{"id": "problem-a", "service": "problem-service", "node": "node-a"},
		"gateway":        map[string]any{"origin": server.URL},
		"bindings": map[string]any{
			"storage_head":   map[string]any{"binding_id": "head-a", "api_id": "storage.object.head", "base_path": "/internal/apis/storage.object.head", "timeout_ms": 300000},
			"storage_delete": map[string]any{"binding_id": "delete-a", "api_id": "storage.object.delete", "base_path": "/internal/apis/storage.object.delete", "timeout_ms": 300000},
		},
		"credential_file": tokenPath, "generation": 4,
	}
	encoded, _ := json.Marshal(contextDocument)
	if err := os.WriteFile(contextPath, encoded, 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", contextPath)
	boundStore, err := artifactgc.NewBoundObjectStore("problems")
	if err != nil {
		t.Fatal(err)
	}
	collector := artifactgc.Collector{
		Ledger: artifactgc.PostgresLedger{DB: db}, Store: boundStore,
		Retention: 7 * 24 * time.Hour, Delete: true,
	}
	report, err := collector.Run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	deleted := map[string]bool{}
	for _, uri := range report.Deleted {
		deleted[uri] = true
	}
	if len(report.Deleted) != 2 || !deleted[orphan.URI] || !deleted[synced[0].StoragePath] ||
		objects.has(orphan.SHA256) || objects.hasKey(problemstorage.ProblemContentObjectKey(1, synced[0].Sha256)) {
		t.Fatalf("rolled-back artifact was not collected through bound Storage: %#v", report)
	}
	if objects.boundDeleteCalls != 2 || objects.unauthorizedCalls != 0 {
		t.Fatalf("GC bypassed workload Gateway binding: %#v", objects)
	}

	// A committed revision is never an orphan. Reinsert a stale intent to model
	// an expand-first upgrade residue and prove the ledger query still excludes
	// the linked object without consulting Judge.
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "")
	if err := os.WriteFile(packageFile, []byte("title: linked\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	linked, err := problemstorage.PublishPackageArtifactTracked(ctx, storageCfg, 2, packageRoot, repo)
	if err != nil {
		t.Fatal(err)
	}
	if err := repo.InTransaction(ctx, func(txRepo *Repository) error {
		if _, err := txRepo.db.Exec(ctx, `INSERT INTO problem_package_revisions(artifact_uri, artifact_sha256, artifact_size_bytes) VALUES($1,$2,$3)`, linked.URI, linked.SHA256, linked.SizeBytes); err != nil {
			return err
		}
		return txRepo.ResolveArtifactUploadIntent(ctx, linked)
	}); err != nil {
		t.Fatal(err)
	}
	old := time.Now().Add(-8 * 24 * time.Hour)
	if _, err := db.Exec(ctx, `
INSERT INTO problem_artifact_upload_intents(
 artifact_uri, artifact_sha256, artifact_size_bytes, status, retry_after, created_at, updated_at
) VALUES($1,$2,$3,'PENDING',$4,$4,$4)
`, linked.URI, linked.SHA256, linked.SizeBytes, old); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", contextPath)
	report, err = collector.Run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if report.Scanned != 1 || report.Referenced != 1 || len(report.Deleted) != 0 || !objects.has(linked.SHA256) {
		t.Fatalf("linked revision intent was not retired without deleting its object: report=%#v present=%v", report, objects.has(linked.SHA256))
	}

	// A failed update may upload bytes whose content-addressed object is
	// already referenced by the current problem_files projection. Its fresh
	// intent must be retired after retention, while the referenced object is
	// never sent to conditional DELETE.
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "")
	reusedFiles, err := problemstorage.SyncProblemFiles(ctx, storageCfg, 3, []packagefs.IndexedFile{{
		LogicalPath: "problem.yaml", StoragePath: packageFile, MimeType: "application/yaml",
	}}, repo)
	if err != nil {
		t.Fatal(err)
	}
	if err := repo.InTransaction(ctx, func(txRepo *Repository) error {
		file := reusedFiles[0]
		if _, err := txRepo.db.Exec(ctx, `INSERT INTO problem_files(storage_path, sha256, size_bytes) VALUES($1,$2,$3)`, file.StoragePath, file.Sha256, file.SizeBytes); err != nil {
			return err
		}
		return txRepo.ResolveProblemFileUploadIntents(ctx, reusedFiles)
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := problemstorage.SyncProblemFiles(ctx, storageCfg, 3, []packagefs.IndexedFile{{
		LogicalPath: "problem.yaml", StoragePath: packageFile, MimeType: "application/yaml",
	}}, repo); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", contextPath)
	old = time.Now().Add(-8 * 24 * time.Hour)
	if _, err := db.Exec(ctx, `UPDATE problem_artifact_upload_intents SET updated_at=$2, retry_after=$2 WHERE artifact_uri=$1`, reusedFiles[0].StoragePath, old); err != nil {
		t.Fatal(err)
	}
	deleteCallsBefore := objects.boundDeleteCalls
	report, err = collector.Run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	reusedKey := problemstorage.ProblemContentObjectKey(3, reusedFiles[0].Sha256)
	if report.Referenced != 1 || len(report.Deleted) != 0 ||
		objects.boundDeleteCalls != deleteCallsBefore || !objects.hasKey(reusedKey) {
		t.Fatalf("failed-update intent for a reused object was not retired safely: report=%#v", report)
	}
	if err := db.QueryRow(ctx, `SELECT COUNT(*) FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, reusedFiles[0].StoragePath).Scan(&pendingCount); err != nil {
		t.Fatal(err)
	}
	if pendingCount != 0 {
		t.Fatalf("referenced reused-object intent was leaked: %d", pendingCount)
	}

	// Resolving is identity-aware: a row that merely reuses the URI but has
	// drifted SHA/size cannot erase the upload evidence.
	mismatchDigest := strings.Repeat("8", 64)
	mismatchRef := eventing.ArtifactRef{
		URI:    "storage://problems/problem-4-objects-sha256-" + mismatchDigest,
		SHA256: mismatchDigest, SizeBytes: 8,
	}
	if err := repo.RegisterArtifactUploadIntent(ctx, mismatchRef); err != nil {
		t.Fatal(err)
	}
	if err := repo.MarkArtifactUploadCompleted(ctx, mismatchRef); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(ctx, `INSERT INTO problem_files(storage_path, sha256, size_bytes) VALUES($1,$2,9)`, mismatchRef.URI, strings.Repeat("9", 64)); err != nil {
		t.Fatal(err)
	}
	if err := repo.ResolveArtifactUploadIntent(ctx, mismatchRef); !errors.Is(err, ErrArtifactIntentUnreferenced) {
		t.Fatalf("identity-mismatched problem_files reference resolved an upload intent: %v", err)
	}
	var mismatchStatus string
	if err := db.QueryRow(ctx, `SELECT status FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, mismatchRef.URI).Scan(&mismatchStatus); err != nil || mismatchStatus != "PENDING" {
		t.Fatalf("identity-mismatched upload evidence was not retained: status=%s err=%v", mismatchStatus, err)
	}
	if _, err := db.Exec(ctx, `DELETE FROM problem_files WHERE storage_path=$1`, mismatchRef.URI); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(ctx, `DELETE FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, mismatchRef.URI); err != nil {
		t.Fatal(err)
	}

	// NEEDS_ATTENTION is operator-owned, not an expired lease. Publishers must
	// surface the terminal condition instead of overwriting it or promising a
	// short automatic retry.
	attention := eventing.ArtifactRef{
		URI:    "storage://problems/package-sha256-" + strings.Repeat("6", 64) + ".zip",
		SHA256: strings.Repeat("6", 64), SizeBytes: 6, ContentType: "application/zip",
	}
	if err := repo.RegisterArtifactUploadIntent(ctx, attention); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(ctx, `UPDATE problem_artifact_upload_intents SET updated_at=$2 WHERE artifact_uri=$1`, attention.URI, old); err != nil {
		t.Fatal(err)
	}
	attentionClaim, err := (artifactgc.PostgresLedger{DB: db}).Claim(ctx, time.Now().Add(-7*24*time.Hour), time.Minute)
	if err != nil || attentionClaim == nil || attentionClaim.URI != attention.URI {
		t.Fatalf("claim terminal fixture: claim=%#v err=%v", attentionClaim, err)
	}
	if err := (artifactgc.PostgresLedger{DB: db}).Quarantine(ctx, *attentionClaim, artifactgc.FailureDetail{
		Message: "operator review required", Stage: "identity", Kind: artifactgc.FailureKindObjectIdentityMismatch, Deterministic: true,
	}); err != nil {
		t.Fatal(err)
	}
	if err := repo.RegisterArtifactUploadIntent(ctx, attention); !errors.Is(err, ErrArtifactNeedsAttention) {
		t.Fatalf("publisher did not surface terminal artifact state: %v", err)
	}
	if err := repo.ResolveArtifactUploadIntent(ctx, attention); !errors.Is(err, ErrArtifactNeedsAttention) {
		t.Fatalf("link transaction did not surface terminal artifact state: %v", err)
	}
	if _, err := db.Exec(ctx, `DELETE FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, attention.URI); err != nil {
		t.Fatal(err)
	}

	// Even an expired DELETING lease remains GC-owned: a delayed provider
	// response may still arrive. Publishers and link transactions must wait for
	// GC to reclaim/finish rather than making the object visible again.
	exclusive := eventing.ArtifactRef{
		URI:    "storage://problems/package-sha256-" + strings.Repeat("7", 64) + ".zip",
		SHA256: strings.Repeat("7", 64), SizeBytes: 7, ContentType: "application/zip",
	}
	if err := repo.RegisterArtifactUploadIntent(ctx, exclusive); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(ctx, `UPDATE problem_artifact_upload_intents SET updated_at=$2 WHERE artifact_uri=$1`, exclusive.URI, old); err != nil {
		t.Fatal(err)
	}
	claim, err := (artifactgc.PostgresLedger{DB: db}).Claim(ctx, time.Now().Add(-7*24*time.Hour), time.Minute)
	if err != nil || claim == nil || claim.URI != exclusive.URI {
		t.Fatalf("claim exclusive orphan: claim=%#v err=%v", claim, err)
	}
	ledger := artifactgc.PostgresLedger{DB: db}
	if err := ledger.Retry(ctx, *claim, artifactgc.FailureDetail{Message: "provider timed out", Stage: "inspect", Kind: artifactgc.FailureKindTransient}, 0); err != nil {
		t.Fatal(err)
	}
	if err := repo.RegisterArtifactUploadIntent(ctx, exclusive); !errors.Is(err, ErrArtifactGCInProgress) {
		t.Fatalf("publisher stole expired deleting intent: %v", err)
	}
	if err := repo.ResolveArtifactUploadIntent(ctx, exclusive); !errors.Is(err, ErrArtifactGCInProgress) {
		t.Fatalf("link transaction stole expired deleting intent: %v", err)
	}
	// Model the collector crashing after the provider request and the full
	// isolation lease subsequently expiring. Retry deliberately does not shorten
	// claim_until: doing so could overlap a late conditional DELETE with a new
	// publisher for the same content-addressed URI.
	if _, err := db.Exec(ctx, `
UPDATE problem_artifact_upload_intents
SET claim_until = NOW() - INTERVAL '1 second'
WHERE artifact_uri = $1 AND status = 'DELETING' AND claim_token = $2
`, exclusive.URI, claim.ClaimToken); err != nil {
		t.Fatal(err)
	}
	reclaimed, err := ledger.Claim(ctx, time.Now().Add(-7*24*time.Hour), time.Minute)
	if err != nil || reclaimed == nil || reclaimed.ClaimToken == claim.ClaimToken {
		t.Fatalf("GC did not exclusively reclaim expired delete: old=%#v new=%#v err=%v", claim, reclaimed, err)
	}
	if err := ledger.CompleteDeleted(ctx, *reclaimed); err != nil {
		t.Fatal(err)
	}
	if err := repo.RegisterArtifactUploadIntent(ctx, exclusive); err != nil {
		t.Fatalf("publisher did not resume after GC completed: %v", err)
	}
}

type integrationObjects struct {
	mu                sync.Mutex
	values            map[string][]byte
	boundDeleteCalls  int
	unauthorizedCalls int
}

func (s *integrationObjects) serve(w http.ResponseWriter, r *http.Request) {
	key := filepath.Base(r.URL.Path)
	bound := strings.HasPrefix(r.URL.Path, "/internal/apis/")
	if bound && r.Header.Get("Authorization") != "Bearer deployment-workload-token" {
		s.mu.Lock()
		s.unauthorizedCalls++
		s.mu.Unlock()
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	switch r.Method {
	case http.MethodPut:
		body, _ := io.ReadAll(r.Body)
		s.values[key] = body
		digest := sha256.Sum256(body)
		_ = json.NewEncoder(w).Encode(map[string]any{"sha256": hex.EncodeToString(digest[:]), "size_bytes": len(body)})
	case http.MethodHead:
		body, ok := s.values[key]
		if !ok {
			w.Header().Set(storagecontract.ResultHeader, storagecontract.ResultObjectNotFound)
			w.WriteHeader(http.StatusNotFound)
			return
		}
		digest := sha256.Sum256(body)
		w.Header().Set(storagecontract.ResultHeader, storagecontract.ResultPresent)
		w.Header().Set("X-OJOS-Object-Sha256", hex.EncodeToString(digest[:]))
		w.Header().Set("Content-Length", fmt.Sprintf("%d", len(body)))
	case http.MethodDelete:
		body, ok := s.values[key]
		if !ok {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		digest := sha256.Sum256(body)
		if r.Header.Get("X-OJOS-Expected-Sha256") != hex.EncodeToString(digest[:]) || r.Header.Get("X-OJOS-Expected-Size") != fmt.Sprintf("%d", len(body)) {
			w.WriteHeader(http.StatusPreconditionFailed)
			return
		}
		delete(s.values, key)
		if bound {
			s.boundDeleteCalls++
		}
		w.Header().Set(storagecontract.ResultHeader, storagecontract.ResultDeleted)
		_ = json.NewEncoder(w).Encode(map[string]any{"deleted": true})
	default:
		w.WriteHeader(http.StatusMethodNotAllowed)
	}
}

func (s *integrationObjects) has(digest string) bool {
	return s.hasKey("package-sha256-" + digest + ".zip")
}

func (s *integrationObjects) hasKey(key string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, ok := s.values[key]
	return ok
}

func artifactIntentPool(t *testing.T, ctx context.Context, databaseURL, schema string) *pgxpool.Pool {
	t.Helper()
	cfg, err := pgxpool.ParseConfig(databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	cfg.ConnConfig.RuntimeParams = map[string]string{"search_path": schema}
	db, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	return db
}
