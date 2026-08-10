package repository

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/alicebob/miniredis/v2"
	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
)

func TestRealTaskReportReceiptAndResultOutboxTransaction(t *testing.T) {
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
	schema := fmt.Sprintf("ojos_judge_result_%d", time.Now().UTC().UnixNano())
	if _, err := admin.Exec(ctx, "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
		t.Fatal(err)
	}
	defer admin.Exec(context.Background(), "DROP SCHEMA IF EXISTS "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
	db := poolWithSearchPath(t, ctx, postgresURL, schema)
	defer db.Close()
	createJudgeResultTestSchema(t, ctx, db)

	if _, err := db.Exec(ctx, `
INSERT INTO judge_workers(worker_id) VALUES('worker-a');
INSERT INTO submissions(id, status) VALUES(1, 'JUDGING'), (2, 'JUDGING'), (3, 'JUDGING');
INSERT INTO judge_tasks(task_id, submission_id, worker_id, lease_version, lease_expires_at, status)
VALUES
    ('sub-1', 1, 'worker-a', 4, NOW() + interval '30 seconds', 'RUNNING'),
    ('sub-2', 2, 'worker-a', 5, NOW() - interval '1 millisecond', 'RUNNING'),
    ('sub-3', 3, 'worker-a', 6, NOW() + interval '30 seconds', 'RUNNING');
`); err != nil {
		t.Fatal(err)
	}
	repo := New(db)
	digest := strings.Repeat("a", 64)
	transition := TaskSuccessTransition{
		Status:        "ACCEPTED",
		Score:         100,
		TimeMS:        12,
		MemoryKB:      2048,
		Message:       "accepted",
		ResultPath:    "storage://submissions/judge-results/1/4/result.json",
		PayloadSHA256: digest,
		OutboxEventID: "judge-result-sub-1-4",
		OutboxPayload: []byte(`{"type":"judge.result.submitted","task_id":"sub-1","status":"ACCEPTED"}`),
	}
	if err := repo.MarkTaskSucceeded(ctx, "sub-1", "worker-a", 4, transition); err != nil {
		t.Fatalf("commit terminal result and outbox: %v", err)
	}
	var taskStatus, submissionStatus string
	var receiptCount, outboxCount int
	if err := db.QueryRow(ctx, `SELECT status FROM judge_tasks WHERE task_id='sub-1'`).Scan(&taskStatus); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(ctx, `SELECT status FROM submissions WHERE id=1`).Scan(&submissionStatus); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(ctx, `SELECT COUNT(*) FROM judge_task_report_receipts WHERE task_id='sub-1'`).Scan(&receiptCount); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(ctx, `SELECT COUNT(*) FROM judge_result_outbox WHERE task_id='sub-1'`).Scan(&outboxCount); err != nil {
		t.Fatal(err)
	}
	if taskStatus != "SUCCEEDED" || submissionStatus != "ACCEPTED" || receiptCount != 1 || outboxCount != 1 {
		t.Fatalf("terminal transaction incomplete: task=%s submission=%s receipt=%d outbox=%d", taskStatus, submissionStatus, receiptCount, outboxCount)
	}
	if err := repo.MarkTaskSucceeded(ctx, "sub-1", "worker-a", 4, transition); !errors.Is(err, ErrTaskTransitionAlreadySaved) {
		t.Fatalf("exact duplicate must replay receipt, got %v", err)
	}
	conflicting := transition
	conflicting.PayloadSHA256 = strings.Repeat("b", 64)
	conflicting.OutboxEventID = "judge-result-conflict"
	if err := repo.MarkTaskSucceeded(ctx, "sub-1", "worker-a", 4, conflicting); !errors.Is(err, ErrTaskLeaseInvalid) {
		t.Fatalf("conflicting same-lease payload must fail closed, got %v", err)
	}

	expired := TaskFailureTransition{
		Status:        "SYSTEM_ERROR",
		Message:       "late worker",
		PayloadSHA256: strings.Repeat("c", 64),
		OutboxEventID: "judge-result-expired",
		OutboxPayload: []byte(`{"type":"judge.result.submitted","task_id":"sub-2","status":"SYSTEM_ERROR"}`),
	}
	if _, err := repo.MarkTaskFailed(ctx, "sub-2", "worker-a", 5, expired); !errors.Is(err, ErrTaskLeaseInvalid) {
		t.Fatalf("expired lease must not create its first receipt, got %v", err)
	}
	if err := db.QueryRow(ctx, `SELECT COUNT(*) FROM judge_task_report_receipts WHERE task_id='sub-2'`).Scan(&receiptCount); err != nil {
		t.Fatal(err)
	}
	if receiptCount != 0 {
		t.Fatalf("expired first report created %d receipts", receiptCount)
	}

	if _, err := db.Exec(ctx, `DROP TABLE judge_result_outbox`); err != nil {
		t.Fatal(err)
	}
	rollback := TaskSuccessTransition{
		Status:        "ACCEPTED",
		Score:         100,
		PayloadSHA256: strings.Repeat("d", 64),
		OutboxEventID: "judge-result-rollback",
		OutboxPayload: []byte(`{"type":"judge.result.submitted","task_id":"sub-3","status":"ACCEPTED"}`),
	}
	if err := repo.MarkTaskSucceeded(ctx, "sub-3", "worker-a", 6, rollback); err == nil {
		t.Fatal("missing outbox table must roll back terminal mutation")
	}
	if err := db.QueryRow(ctx, `SELECT status FROM judge_tasks WHERE task_id='sub-3'`).Scan(&taskStatus); err != nil {
		t.Fatal(err)
	}
	if taskStatus != "RUNNING" {
		t.Fatalf("task transition survived failed outbox insert: %s", taskStatus)
	}
}

func TestRealJudgeResultOutboxRelayRecoversAfterRedisFailure(t *testing.T) {
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
	schema := fmt.Sprintf("ojos_judge_relay_%d", time.Now().UTC().UnixNano())
	if _, err := admin.Exec(ctx, "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
		t.Fatal(err)
	}
	defer admin.Exec(context.Background(), "DROP SCHEMA IF EXISTS "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
	db := poolWithSearchPath(t, ctx, postgresURL, schema)
	defer db.Close()
	createJudgeResultTestSchema(t, ctx, db)
	if _, err := db.Exec(ctx, `
INSERT INTO judge_task_report_receipts(task_id, lease_version, worker_id, report_kind, payload_sha256, response_status, event_id)
VALUES('sub-relay', 1, 'worker-a', 'result', repeat('a', 64), 'ACCEPTED', 'event-relay');
INSERT INTO judge_result_outbox(event_id, task_id, lease_version, payload_sha256, payload)
VALUES('event-relay', 'sub-relay', 1, repeat('a', 64), '{"type":"judge.result.submitted","task_id":"sub-relay","status":"ACCEPTED"}');
`); err != nil {
		t.Fatal(err)
	}

	broken := redis.NewClient(&redis.Options{Addr: "127.0.0.1:1", DialTimeout: 50 * time.Millisecond})
	relay := &JudgeResultOutboxRelay{DB: db, Redis: broken, Stream: "judge-results-test", RelayID: "test"}
	if published, err := relay.PublishBatch(ctx); err != nil || published != 0 {
		t.Fatalf("Redis failure must defer the durable row, published=%d err=%v", published, err)
	}
	var publishedAt *time.Time
	if err := db.QueryRow(ctx, `SELECT published_at FROM judge_result_outbox WHERE event_id='event-relay'`).Scan(&publishedAt); err != nil {
		t.Fatal(err)
	}
	if publishedAt != nil {
		t.Fatal("failed Redis write marked outbox row published")
	}

	redisServer := miniredis.RunT(t)
	redisClient := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	defer redisClient.Close()
	relay.Redis = redisClient
	if _, err := db.Exec(ctx, `UPDATE judge_result_outbox SET available_at=NOW(), lease_owner=NULL, lease_until=NULL`); err != nil {
		t.Fatal(err)
	}
	if published, err := relay.PublishBatch(ctx); err != nil || published != 1 {
		t.Fatalf("recovered relay: published=%d err=%v", published, err)
	}
	entries, err := redisClient.XRange(ctx, "judge-results-test", "-", "+").Result()
	if err != nil || len(entries) != 1 {
		t.Fatalf("result stream delivery: entries=%#v err=%v", entries, err)
	}
	if entries[0].Values["event_id"] != "event-relay" || entries[0].Values["task_id"] != "sub-relay" {
		t.Fatalf("unexpected relayed event: %#v", entries[0].Values)
	}
}

func createJudgeResultTestSchema(t *testing.T, ctx context.Context, db *pgxpool.Pool) {
	t.Helper()
	_, err := db.Exec(ctx, `
CREATE TABLE judge_workers(worker_id TEXT PRIMARY KEY);
CREATE TABLE submissions(
    id BIGINT PRIMARY KEY,
    status TEXT NOT NULL,
    score INT NOT NULL DEFAULT 0,
    time_ms INT NOT NULL DEFAULT 0,
    memory_kb INT NOT NULL DEFAULT 0,
    message TEXT NOT NULL DEFAULT '',
    result_path TEXT NOT NULL DEFAULT '',
    judged_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE judge_tasks(
    task_id TEXT PRIMARY KEY,
    submission_id BIGINT NOT NULL REFERENCES submissions(id),
    worker_id TEXT REFERENCES judge_workers(worker_id),
    lease_version INT NOT NULL,
    lease_expires_at TIMESTAMPTZ,
    heartbeat_at TIMESTAMPTZ,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    attempt INT NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    error_message TEXT NOT NULL DEFAULT '',
    result_payload_sha256 TEXT NOT NULL DEFAULT '',
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
