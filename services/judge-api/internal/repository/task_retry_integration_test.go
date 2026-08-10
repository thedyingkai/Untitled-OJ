package repository

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
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

func TestRealRetryableFailureReturnsEffectiveBackoffAndExhaustion(t *testing.T) {
	postgresURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if postgresURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	db, cleanup := taskRetryTestDatabase(t, ctx, postgresURL, "explicit_failure")
	defer cleanup()

	if _, err := db.Exec(ctx, `
INSERT INTO judge_workers(worker_id) VALUES('worker-a');
INSERT INTO submissions(id, problem_id, language, status) VALUES(1, 7, 'cpp17', 'PENDING');
INSERT INTO judge_tasks(task_id, submission_id, problem_id, language, status, available_at)
VALUES('sub-1', 1, 7, 'cpp17', 'PENDING', NOW());
`); err != nil {
		t.Fatal(err)
	}
	repo := New(db)

	for attempt, expectedDelay := range []time.Duration{time.Second, 5 * time.Second, 30 * time.Second} {
		leases, err := repo.ClaimTasks(ctx, "worker-a", []string{"cpp17"}, 1, time.Minute, []string{"sub-1"})
		if err != nil || len(leases) != 1 || leases[0].Attempt != attempt+1 {
			t.Fatalf("claim attempt %d: leases=%#v err=%v", attempt+1, leases, err)
		}
		transition := retryableFailureTransition(t, leases[0], "artifact download failed")
		outcome, err := repo.MarkTaskFailed(ctx, leases[0].TaskID, leases[0].WorkerID, leases[0].LeaseVersion, transition)
		if err != nil {
			t.Fatalf("fail attempt %d: %v", attempt+1, err)
		}
		if outcome.Status != "PENDING" || !outcome.RetryScheduled || outcome.AvailableAt == nil {
			t.Fatalf("attempt %d outcome: %#v", attempt+1, outcome)
		}
		remaining := time.Until(*outcome.AvailableAt)
		if remaining < expectedDelay-500*time.Millisecond || remaining > expectedDelay+500*time.Millisecond {
			t.Fatalf("attempt %d backoff=%s want about %s", attempt+1, remaining, expectedDelay)
		}
		duplicate, err := repo.MarkTaskFailed(ctx, leases[0].TaskID, leases[0].WorkerID, leases[0].LeaseVersion, transition)
		if !errors.Is(err, ErrTaskTransitionAlreadySaved) || duplicate.Status != "PENDING" || !duplicate.RetryScheduled || !duplicate.AlreadySaved {
			t.Fatalf("attempt %d duplicate outcome=%#v err=%v", attempt+1, duplicate, err)
		}
		if leases, err := repo.ClaimTasks(ctx, "worker-a", []string{"cpp17"}, 1, time.Minute, []string{"sub-1"}); err != nil || len(leases) != 0 {
			t.Fatalf("attempt %d was claimable before backoff: %#v err=%v", attempt+1, leases, err)
		}
		if _, err := db.Exec(ctx, `UPDATE judge_tasks SET available_at=NOW() WHERE task_id='sub-1'`); err != nil {
			t.Fatal(err)
		}
	}

	leases, err := repo.ClaimTasks(ctx, "worker-a", []string{"cpp17"}, 1, time.Minute, []string{"sub-1"})
	if err != nil || len(leases) != 1 || leases[0].Attempt != 4 {
		t.Fatalf("fourth claim: leases=%#v err=%v", leases, err)
	}
	transition := retryableFailureTransition(t, leases[0], "artifact download failed")
	outcome, err := repo.MarkTaskFailed(ctx, leases[0].TaskID, leases[0].WorkerID, leases[0].LeaseVersion, transition)
	if err != nil {
		t.Fatal(err)
	}
	if outcome.Status != "SYSTEM_ERROR" || outcome.RetryScheduled || outcome.AvailableAt != nil {
		t.Fatalf("fourth failure did not exhaust: %#v", outcome)
	}
	duplicate, err := repo.MarkTaskFailed(ctx, leases[0].TaskID, leases[0].WorkerID, leases[0].LeaseVersion, transition)
	if !errors.Is(err, ErrTaskTransitionAlreadySaved) || duplicate.Status != "SYSTEM_ERROR" || duplicate.RetryScheduled || !duplicate.AlreadySaved {
		t.Fatalf("terminal duplicate outcome=%#v err=%v", duplicate, err)
	}
	var taskStatus, submissionStatus, receiptStatus string
	var outbox int
	if err := db.QueryRow(ctx, `SELECT status FROM judge_tasks WHERE task_id='sub-1'`).Scan(&taskStatus); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(ctx, `SELECT status FROM submissions WHERE id=1`).Scan(&submissionStatus); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(ctx, `SELECT response_status FROM judge_task_report_receipts WHERE task_id='sub-1' AND lease_version=4`).Scan(&receiptStatus); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(ctx, `SELECT COUNT(*) FROM judge_result_outbox WHERE task_id='sub-1' AND lease_version=4`).Scan(&outbox); err != nil {
		t.Fatal(err)
	}
	if taskStatus != "FAILED" || submissionStatus != "SYSTEM_ERROR" || receiptStatus != "SYSTEM_ERROR" || outbox != 1 {
		t.Fatalf("terminal state: task=%s submission=%s receipt=%s outbox=%d", taskStatus, submissionStatus, receiptStatus, outbox)
	}
}

func TestRealTaskLeaseExpiryUsesBackoffAndStopsAfterFourthAttempt(t *testing.T) {
	postgresURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if postgresURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	db, cleanup := taskRetryTestDatabase(t, ctx, postgresURL, "sequence")
	defer cleanup()

	if _, err := db.Exec(ctx, `
INSERT INTO judge_workers(worker_id) VALUES('worker-a');
INSERT INTO submissions(id, problem_id, language, status) VALUES(1, 7, 'cpp17', 'PENDING');
INSERT INTO judge_tasks(task_id, submission_id, problem_id, language, status, available_at)
VALUES('sub-1', 1, 7, 'cpp17', 'PENDING', NOW());
`); err != nil {
		t.Fatal(err)
	}
	repo := New(db)

	for attempt, expectedDelay := range []time.Duration{time.Second, 5 * time.Second, 30 * time.Second} {
		leases, err := repo.ClaimTasks(ctx, "worker-a", []string{"cpp17"}, 1, time.Minute, []string{"sub-1"})
		if err != nil {
			t.Fatalf("claim attempt %d: %v", attempt+1, err)
		}
		if len(leases) != 1 || leases[0].Attempt != attempt+1 {
			t.Fatalf("claim attempt %d returned %#v", attempt+1, leases)
		}
		if _, err := db.Exec(ctx, `UPDATE judge_tasks SET lease_expires_at=NOW()-interval '1 second' WHERE task_id='sub-1'`); err != nil {
			t.Fatal(err)
		}
		if recovered, err := repo.RecoverStaleTasks(ctx); err != nil || recovered != 1 {
			t.Fatalf("recover attempt %d: recovered=%d err=%v", attempt+1, recovered, err)
		}

		var status string
		var remaining time.Duration
		if err := db.QueryRow(ctx, `SELECT status, available_at-NOW() FROM judge_tasks WHERE task_id='sub-1'`).Scan(&status, &remaining); err != nil {
			t.Fatal(err)
		}
		if status != "PENDING" || remaining < expectedDelay-500*time.Millisecond || remaining > expectedDelay+500*time.Millisecond {
			t.Fatalf("attempt %d schedule: status=%s remaining=%s want about %s", attempt+1, status, remaining, expectedDelay)
		}
		if leases, err := repo.ClaimTasks(ctx, "worker-a", []string{"cpp17"}, 1, time.Minute, []string{"sub-1"}); err != nil || len(leases) != 0 {
			t.Fatalf("task was claimable before backoff: leases=%#v err=%v", leases, err)
		}
		if _, err := db.Exec(ctx, `UPDATE judge_tasks SET available_at=NOW() WHERE task_id='sub-1'`); err != nil {
			t.Fatal(err)
		}
	}

	leases, err := repo.ClaimTasks(ctx, "worker-a", []string{"cpp17"}, 1, time.Minute, []string{"sub-1"})
	if err != nil || len(leases) != 1 || leases[0].Attempt != 4 {
		t.Fatalf("fourth claim: leases=%#v err=%v", leases, err)
	}
	if _, err := db.Exec(ctx, `UPDATE judge_tasks SET lease_expires_at=NOW()-interval '1 second' WHERE task_id='sub-1'`); err != nil {
		t.Fatal(err)
	}
	if recovered, err := repo.RecoverStaleTasks(ctx); err != nil || recovered != 1 {
		t.Fatalf("exhaust fourth attempt: recovered=%d err=%v", recovered, err)
	}

	var taskStatus, submissionStatus string
	var receipts, outbox int
	if err := db.QueryRow(ctx, `SELECT status FROM judge_tasks WHERE task_id='sub-1'`).Scan(&taskStatus); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(ctx, `SELECT status FROM submissions WHERE id=1`).Scan(&submissionStatus); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(ctx, `SELECT COUNT(*) FROM judge_task_report_receipts WHERE task_id='sub-1'`).Scan(&receipts); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(ctx, `SELECT COUNT(*) FROM judge_result_outbox WHERE task_id='sub-1'`).Scan(&outbox); err != nil {
		t.Fatal(err)
	}
	if taskStatus != "FAILED" || submissionStatus != "SYSTEM_ERROR" || receipts != 1 || outbox != 1 {
		t.Fatalf("exhausted state: task=%s submission=%s receipts=%d outbox=%d", taskStatus, submissionStatus, receipts, outbox)
	}
	if leases, err := repo.ClaimTasks(ctx, "worker-a", []string{"cpp17"}, 1, time.Minute, []string{"sub-1"}); err != nil || len(leases) != 0 {
		t.Fatalf("exhausted task was claimed: leases=%#v err=%v", leases, err)
	}
	if recovered, err := repo.RecoverStaleTasks(ctx); err != nil || recovered != 0 {
		t.Fatalf("terminal recovery was not idempotent: recovered=%d err=%v", recovered, err)
	}
}

func TestRealConcurrentStaleRecoveryTransitionsEachLeaseOnce(t *testing.T) {
	postgresURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	if postgresURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	db, cleanup := taskRetryTestDatabase(t, ctx, postgresURL, "concurrent")
	defer cleanup()

	if _, err := db.Exec(ctx, `INSERT INTO judge_workers(worker_id) VALUES('worker-a')`); err != nil {
		t.Fatal(err)
	}
	for id := int64(1); id <= 16; id++ {
		tx, err := db.Begin(ctx)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := tx.Exec(ctx,
			`INSERT INTO submissions(id, problem_id, language, status) VALUES($1, 7, 'cpp17', 'JUDGING')`,
			id,
		); err != nil {
			_ = tx.Rollback(ctx)
			t.Fatal(err)
		}
		if _, err := tx.Exec(ctx, `
INSERT INTO judge_tasks(task_id, submission_id, problem_id, language, worker_id, lease_version, lease_expires_at, attempt, status)
VALUES($1, $2, 7, 'cpp17', 'worker-a', 4, NOW()-interval '1 second', 4, 'RUNNING')
`, fmt.Sprintf("sub-%d", id), id); err != nil {
			_ = tx.Rollback(ctx)
			t.Fatal(err)
		}
		if err := tx.Commit(ctx); err != nil {
			t.Fatal(err)
		}
	}

	repo := New(db)
	results := make(chan int64, 2)
	errors := make(chan error, 2)
	var wait sync.WaitGroup
	for range 2 {
		wait.Add(1)
		go func() {
			defer wait.Done()
			recovered, err := repo.RecoverStaleTasks(ctx)
			results <- recovered
			errors <- err
		}()
	}
	wait.Wait()
	close(results)
	close(errors)
	var total int64
	for recovered := range results {
		total += recovered
	}
	for err := range errors {
		if err != nil {
			t.Fatal(err)
		}
	}
	if total != 16 {
		t.Fatalf("concurrent recovery count: got %d want 16", total)
	}
	var failed, receipts, outbox int
	if err := db.QueryRow(ctx, `SELECT COUNT(*) FROM judge_tasks WHERE status='FAILED'`).Scan(&failed); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(ctx, `SELECT COUNT(*) FROM judge_task_report_receipts`).Scan(&receipts); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(ctx, `SELECT COUNT(*) FROM judge_result_outbox`).Scan(&outbox); err != nil {
		t.Fatal(err)
	}
	if failed != 16 || receipts != 16 || outbox != 16 {
		t.Fatalf("concurrent recovery duplicated or lost transitions: failed=%d receipts=%d outbox=%d", failed, receipts, outbox)
	}
}

func taskRetryTestDatabase(
	t *testing.T,
	ctx context.Context,
	postgresURL string,
	label string,
) (*pgxpool.Pool, func()) {
	t.Helper()
	admin, err := pgxpool.New(ctx, postgresURL)
	if err != nil {
		t.Fatal(err)
	}
	schema := fmt.Sprintf("ojos_judge_retry_%s_%d", label, time.Now().UTC().UnixNano())
	if _, err := admin.Exec(ctx, "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
		admin.Close()
		t.Fatal(err)
	}
	db := poolWithSearchPath(t, ctx, postgresURL, schema)
	createTaskRetryTestSchema(t, ctx, db)
	return db, func() {
		db.Close()
		_, _ = admin.Exec(context.Background(), "DROP SCHEMA IF EXISTS "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
		admin.Close()
	}
}

func createTaskRetryTestSchema(t *testing.T, ctx context.Context, db *pgxpool.Pool) {
	t.Helper()
	_, err := db.Exec(ctx, `
CREATE TABLE judge_workers(
    worker_id TEXT PRIMARY KEY,
    drain BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE TABLE submissions(
    id BIGINT PRIMARY KEY,
    problem_id BIGINT NOT NULL,
    language TEXT NOT NULL,
    status TEXT NOT NULL,
    score INT NOT NULL DEFAULT 0,
    time_ms INT NOT NULL DEFAULT 0,
    memory_kb INT NOT NULL DEFAULT 0,
    message TEXT NOT NULL DEFAULT '',
    judged_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE judge_tasks(
    id BIGSERIAL PRIMARY KEY,
    task_id TEXT NOT NULL UNIQUE,
    submission_id BIGINT NOT NULL UNIQUE REFERENCES submissions(id),
    problem_id BIGINT NOT NULL,
    language TEXT NOT NULL,
    worker_id TEXT REFERENCES judge_workers(worker_id),
    lease_version INT NOT NULL DEFAULT 0,
    lease_expires_at TIMESTAMPTZ,
    heartbeat_at TIMESTAMPTZ,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    attempt INT NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'PENDING',
    error_message TEXT NOT NULL DEFAULT '',
    result_payload_sha256 TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE judge_task_report_receipts(
    task_id TEXT NOT NULL,
    lease_version INT NOT NULL,
    worker_id TEXT NOT NULL,
    report_kind TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    response_status TEXT NOT NULL,
    event_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(task_id, lease_version)
);
CREATE TABLE judge_result_outbox(
    sequence BIGSERIAL PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    task_id TEXT NOT NULL,
    lease_version INT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    payload JSONB NOT NULL,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    attempt_count INT NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_until TIMESTAMPTZ,
    published_at TIMESTAMPTZ,
    last_error TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    FOREIGN KEY(task_id, lease_version) REFERENCES judge_task_report_receipts(task_id, lease_version),
    UNIQUE(task_id, lease_version)
);
`)
	if err != nil {
		t.Fatal(err)
	}
}

func retryableFailureTransition(
	t *testing.T,
	lease TaskLeaseView,
	message string,
) TaskFailureTransition {
	t.Helper()
	payload, err := json.Marshal(map[string]any{
		"type":          "judge.result.submitted",
		"producer":      "judge-api-service",
		"task_id":       lease.TaskID,
		"worker_id":     lease.WorkerID,
		"lease_version": lease.LeaseVersion,
		"status":        "SYSTEM_ERROR",
		"message":       message,
	})
	if err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256([]byte(fmt.Sprintf(
		"%s\x00%s\x00%d\x00%s\x00true",
		lease.TaskID,
		lease.WorkerID,
		lease.LeaseVersion,
		message,
	)))
	payloadSHA256 := hex.EncodeToString(digest[:])
	eventDigest := sha256.Sum256([]byte(
		"judge-result\x00" + lease.TaskID + "\x00" +
			fmt.Sprint(lease.LeaseVersion) + "\x00" + payloadSHA256,
	))
	return TaskFailureTransition{
		Status:        "SYSTEM_ERROR",
		Message:       message,
		Retryable:     true,
		PayloadSHA256: payloadSHA256,
		OutboxEventID: "judge-result-" + hex.EncodeToString(eventDigest[:]),
		OutboxPayload: payload,
	}
}
