package topologyprojection

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
)

// TestPostgresProjectionRestorePreviousIsCASAndIdempotent is an opt-in real
// PostgreSQL contract test. CI can provide AUTH_TOPOLOGY_TEST_DATABASE_URL;
// the existing Auth integration database variable is accepted as a fallback.
func TestPostgresProjectionRestorePreviousIsCASAndIdempotent(t *testing.T) {
	databaseURL := os.Getenv("AUTH_TOPOLOGY_TEST_DATABASE_URL")
	if databaseURL == "" {
		databaseURL = os.Getenv("AUTH_BOOTSTRAP_TEST_DATABASE_URL")
	}
	if databaseURL == "" {
		t.Skip("AUTH_TOPOLOGY_TEST_DATABASE_URL is not configured")
	}
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()

	adminPool, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	defer adminPool.Close()
	schema := fmt.Sprintf("auth_topology_test_%d", time.Now().UnixNano())
	if _, err := adminPool.Exec(ctx, "CREATE SCHEMA "+schema); err != nil {
		t.Fatal(err)
	}
	defer func() { _, _ = adminPool.Exec(context.Background(), "DROP SCHEMA "+schema+" CASCADE") }()

	poolConfig, err := pgxpool.ParseConfig(databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	poolConfig.ConnConfig.RuntimeParams["search_path"] = schema
	pool, err := pgxpool.NewWithConfig(ctx, poolConfig)
	if err != nil {
		t.Fatal(err)
	}
	defer pool.Close()
	migration, err := os.ReadFile(filepath.Join("..", "..", "migrations", "000013_topology_binding_projection.up.sql"))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := pool.Exec(ctx, string(migration)); err != nil {
		t.Fatal(err)
	}

	store := NewStore(pool)
	attemptRevision := "revision-2"
	attempt := authRequest("primary", "operation-2", "binding-new")
	attempt.AttemptedRevisionID = attemptRevision
	attempt.DesiredRevisionID = &attemptRevision
	if err := store.Apply(ctx, attempt); err != nil {
		t.Fatal(err)
	}

	previousRevision := "revision-1"
	restore := authRequest("primary", "operation-2", "binding-previous")
	restore.Action = "restore_previous"
	restore.AttemptedRevisionID = attemptRevision
	restore.DesiredRevisionID = &previousRevision
	if err := store.Apply(ctx, restore); err != nil {
		t.Fatalf("restore failed: %v", err)
	}
	var restoredAt time.Time
	if err := pool.QueryRow(ctx, `SELECT updated_at FROM auth_topology_projections WHERE topology_id = 'primary'`).Scan(&restoredAt); err != nil {
		t.Fatal(err)
	}
	if err := store.Apply(ctx, restore); err != nil {
		t.Fatalf("restore replay failed: %v", err)
	}
	var replayedAt time.Time
	if err := pool.QueryRow(ctx, `SELECT updated_at FROM auth_topology_projections WHERE topology_id = 'primary'`).Scan(&replayedAt); err != nil {
		t.Fatal(err)
	}
	if !replayedAt.Equal(restoredAt) {
		t.Fatalf("idempotent replay rewrote projection: restored=%s replayed=%s", restoredAt, replayedAt)
	}

	newRevision := "revision-3"
	newer := authRequest("primary", "operation-3", "binding-newer")
	newer.AttemptedRevisionID = newRevision
	newer.DesiredRevisionID = &newRevision
	if err := store.Apply(ctx, newer); err != nil {
		t.Fatal(err)
	}
	if err := store.Apply(ctx, restore); err == nil {
		t.Fatal("stale restore was accepted over a newer PostgreSQL projection")
	}
	document, err := store.Get(ctx, "primary")
	if err != nil || document == nil || document.RevisionID != newRevision || document.OperationID != "operation-3" {
		t.Fatalf("stale restore changed PostgreSQL state: document=%v err=%v", document, err)
	}
}
