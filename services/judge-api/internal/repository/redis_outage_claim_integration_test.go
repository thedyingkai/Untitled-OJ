package repository

import (
	"context"
	"os"
	"strings"
	"testing"
	"time"
)

// This opt-in integration test proves the production persistence invariant
// independently of Redis: a task committed to PostgreSQL remains claimable
// when no stream entry exists and the Worker sends an empty task-id filter.
func TestRealPostgresClaimsTaskWithoutRedisSignal(t *testing.T) {
	postgresURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if postgresURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	db, cleanup := taskRetryTestDatabase(t, ctx, postgresURL, "redis_outage")
	defer cleanup()
	if _, err := db.Exec(ctx, `
INSERT INTO judge_workers(worker_id) VALUES('worker-db-poll');
INSERT INTO submissions(id, problem_id, language, status)
VALUES(501, 7001, 'cpp17', 'PENDING');
`); err != nil {
		t.Fatal(err)
	}

	repo := New(db)
	if err := repo.EnsureTaskForSubmission(ctx, 501); err != nil {
		t.Fatalf("persist PostgreSQL judge task: %v", err)
	}
	leases, err := repo.ClaimTasks(
		ctx,
		"worker-db-poll",
		[]string{"cpp17"},
		1,
		time.Minute,
		nil,
	)
	if err != nil {
		t.Fatalf("claim without Redis-derived task IDs: %v", err)
	}
	if len(leases) != 1 || leases[0].TaskID != "sub-501" || leases[0].SubmissionID != 501 {
		t.Fatalf("PostgreSQL task was not claimed during Redis outage: %#v", leases)
	}
}
