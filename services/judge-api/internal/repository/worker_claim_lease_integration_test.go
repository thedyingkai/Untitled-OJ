package repository

import (
	"context"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

func TestRealClaimLeaseFinalizeAndExactCompensation(t *testing.T) {
	postgresURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if postgresURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	admin, err := pgxpool.New(ctx, postgresURL)
	if err != nil {
		t.Fatal(err)
	}
	defer admin.Close()
	schema := fmt.Sprintf("ojos_claim_lease_%d", time.Now().UTC().UnixNano())
	if _, err := admin.Exec(ctx, "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
		t.Fatal(err)
	}
	defer admin.Exec(context.Background(), "DROP SCHEMA IF EXISTS "+pgx.Identifier{schema}.Sanitize()+" CASCADE")

	db := poolWithSearchPath(t, ctx, postgresURL, schema)
	defer db.Close()
	if _, err := db.Exec(ctx, `
CREATE TABLE submissions (
    id BIGINT PRIMARY KEY,
    status TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE judge_tasks (
    id BIGSERIAL PRIMARY KEY,
    task_id TEXT NOT NULL UNIQUE,
    submission_id BIGINT NOT NULL REFERENCES submissions(id),
    problem_id BIGINT NOT NULL,
    language TEXT NOT NULL,
    worker_id TEXT,
    lease_version INTEGER NOT NULL DEFAULT 0,
    lease_expires_at TIMESTAMPTZ,
    heartbeat_at TIMESTAMPTZ,
    attempt INTEGER NOT NULL DEFAULT 0,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    status TEXT NOT NULL,
    error_message TEXT NOT NULL DEFAULT '',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
INSERT INTO submissions(id, status) VALUES (91, 'JUDGING'), (92, 'JUDGING');
INSERT INTO judge_tasks(
    task_id, submission_id, problem_id, language, worker_id,
    lease_version, lease_expires_at, heartbeat_at, attempt, status
) VALUES
    ('sub-91', 91, 1091, 'cpp17', 'worker-a', 1, NOW() - INTERVAL '1 second', NOW(), 1, 'RUNNING'),
    ('sub-92', 92, 1092, 'cpp17', 'worker-b', 2, NOW() + INTERVAL '1 minute', NOW(), 1, 'RUNNING');
`); err != nil {
		t.Fatal(err)
	}

	repo := New(db)
	refreshed, err := repo.RefreshClaimedTaskLease(ctx, "sub-91", "worker-a", 1, 30*time.Second)
	if err != nil {
		t.Fatalf("finalize expired, unexposed lease: %v", err)
	}
	if !refreshed.LeaseExpiresAt.After(time.Now()) {
		t.Fatalf("finalized lease did not receive a future expiry: %s", refreshed.LeaseExpiresAt)
	}

	released, err := repo.ReleaseClaimedTasks(ctx, "worker-a", []TaskLeaseView{
		{TaskID: "sub-91", LeaseVersion: 1},
		// This is the stale identity which preceded worker-b's current lease.
		{TaskID: "sub-92", LeaseVersion: 1},
	}, "claim response was not delivered")
	if err != nil {
		t.Fatalf("release exact claims: %v", err)
	}
	if released != 1 {
		t.Fatalf("expected exactly one owned lease release, got %d", released)
	}

	var status, workerID string
	var expiresAt *time.Time
	var attempt int
	var availableAt time.Time
	if err := db.QueryRow(ctx, `
SELECT status, COALESCE(worker_id, ''), lease_expires_at, attempt, available_at
FROM judge_tasks WHERE task_id = 'sub-91'
`).Scan(&status, &workerID, &expiresAt, &attempt, &availableAt); err != nil {
		t.Fatal(err)
	}
	if status != "PENDING" || workerID != "" || expiresAt != nil || attempt != 0 {
		t.Fatalf("owned unexposed lease was not fully released: status=%s worker=%s expiry=%v attempt=%d", status, workerID, expiresAt, attempt)
	}
	if availableAt.After(time.Now()) {
		t.Fatalf("unexposed claim was not immediately retryable: available_at=%s", availableAt)
	}

	var version int
	if err := db.QueryRow(ctx, `
SELECT status, COALESCE(worker_id, ''), lease_version
FROM judge_tasks WHERE task_id = 'sub-92'
`).Scan(&status, &workerID, &version); err != nil {
		t.Fatal(err)
	}
	if status != "RUNNING" || workerID != "worker-b" || version != 2 {
		t.Fatalf("stale compensation changed the new lease: status=%s worker=%s version=%d", status, workerID, version)
	}
}
