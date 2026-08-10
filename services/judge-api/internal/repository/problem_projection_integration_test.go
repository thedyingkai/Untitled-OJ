package repository

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"

	"ojos-shared/eventing"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
)

// This test is intentionally opt-in for local/CI infrastructure with real
// PostgreSQL and Redis. It proves the cross-database outbox -> stream -> inbox
// -> projection path, including the same handler used by judge-api at runtime.
func TestRealProblemProjectionOutboxStreamInbox(t *testing.T) {
	postgresURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_POSTGRES_URL"))
	redisURL := strings.TrimSpace(os.Getenv("OJOS_EVENTING_TEST_REDIS_URL"))
	if postgresURL == "" || redisURL == "" {
		t.Skip("set OJOS_EVENTING_TEST_POSTGRES_URL and OJOS_EVENTING_TEST_REDIS_URL")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	admin, err := pgxpool.New(ctx, postgresURL)
	if err != nil {
		t.Fatal(err)
	}
	defer admin.Close()
	suffix := time.Now().UTC().UnixNano()
	sourceSchema := fmt.Sprintf("ojos_event_source_%d", suffix)
	judgeSchema := fmt.Sprintf("ojos_event_judge_%d", suffix)
	for _, schema := range []string{sourceSchema, judgeSchema} {
		if _, err := admin.Exec(ctx, "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
			t.Fatal(err)
		}
		defer admin.Exec(context.Background(), "DROP SCHEMA IF EXISTS "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
	}

	sourceDB := poolWithSearchPath(t, ctx, postgresURL, sourceSchema)
	defer sourceDB.Close()
	judgeDB := poolWithSearchPath(t, ctx, postgresURL, judgeSchema)
	defer judgeDB.Close()
	if _, err := sourceDB.Exec(ctx, `
CREATE TABLE integration_outbox (
    sequence BIGSERIAL PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    aggregate_version BIGINT NOT NULL,
    event_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    attempt_count INT NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_until TIMESTAMPTZ,
    published_at TIMESTAMPTZ,
    last_error TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(aggregate_type, aggregate_id, aggregate_version, event_type)
)
`); err != nil {
		t.Fatal(err)
	}
	if _, err := judgeDB.Exec(ctx, `
CREATE TABLE problems (
    id BIGINT PRIMARY KEY,
    package_dir TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'ready',
    visibility TEXT NOT NULL DEFAULT 'public',
    created_by BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    aggregate_version BIGINT NOT NULL DEFAULT 0,
    package_revision BIGINT NOT NULL DEFAULT 0,
    package_artifact_uri TEXT NOT NULL DEFAULT '',
    package_artifact_sha256 TEXT NOT NULL DEFAULT '',
    package_artifact_size_bytes BIGINT NOT NULL DEFAULT 0,
    manifest_sha256 TEXT NOT NULL DEFAULT '',
    projected_event_id TEXT NOT NULL DEFAULT '',
    source_updated_at TIMESTAMPTZ,
    deleted BOOLEAN NOT NULL DEFAULT FALSE,
    problem_no TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    problem_type TEXT NOT NULL DEFAULT 'traditional',
    time_limit_ms INT NOT NULL DEFAULT 1000,
    memory_limit_mb INT NOT NULL DEFAULT 256
);
CREATE TABLE submissions (
    id BIGSERIAL PRIMARY KEY,
    problem_id BIGINT NOT NULL,
    user_id BIGINT NOT NULL,
    language TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'PENDING',
    score INT NOT NULL DEFAULT 0,
    time_ms INT NOT NULL DEFAULT 0,
    memory_kb INT NOT NULL DEFAULT 0,
    message TEXT,
    code_path TEXT NOT NULL DEFAULT '',
    code_sha256 TEXT NOT NULL DEFAULT '',
    result_path TEXT NOT NULL DEFAULT '',
    judged_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    cancelled_by BIGINT,
    cancel_reason TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    problem_aggregate_version BIGINT NOT NULL DEFAULT 0,
    problem_package_revision BIGINT NOT NULL DEFAULT 0,
    problem_artifact_uri TEXT NOT NULL DEFAULT '',
    problem_artifact_sha256 TEXT NOT NULL DEFAULT '',
    problem_artifact_size_bytes BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE integration_inbox (
    consumer_name TEXT NOT NULL,
    event_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    received_at TIMESTAMPTZ NOT NULL,
    processed_at TIMESTAMPTZ,
    PRIMARY KEY(consumer_name, event_id)
);
CREATE TABLE integration_dead_letters (
    consumer_name TEXT NOT NULL,
    event_id TEXT NOT NULL,
    stream_entry_id TEXT NOT NULL,
    payload JSONB NOT NULL,
    attempts INT NOT NULL DEFAULT 1,
    last_error TEXT NOT NULL,
    first_failed_at TIMESTAMPTZ NOT NULL,
    last_failed_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(consumer_name, event_id)
)
`); err != nil {
		t.Fatal(err)
	}
	if _, err := judgeDB.Exec(ctx, `
INSERT INTO problems(id, package_dir, status, visibility, created_by)
VALUES(88, '/legacy/problem-88', 'ready', 'public', 9)
`); err != nil {
		t.Fatal(err)
	}
	if _, err := New(judgeDB).CreateSubmission(ctx, 88, 99, "cpp17"); !errors.Is(err, ErrProblemProjectionNotReady) {
		t.Fatalf("default repository accepted a legacy problem during backfill: %v", err)
	}
	legacySubmissionID, err := New(judgeDB, WithLegacyProblemPackageDir(true)).CreateSubmission(ctx, 88, 99, "cpp17")
	if err != nil {
		t.Fatalf("explicit development repository compatibility was rejected: %v", err)
	}
	if _, err := judgeDB.Exec(ctx, `DELETE FROM submissions WHERE id = $1`, legacySubmissionID); err != nil {
		t.Fatal(err)
	}
	if _, err := judgeDB.Exec(ctx, `DELETE FROM problems WHERE id = 88`); err != nil {
		t.Fatal(err)
	}

	redisOptions, err := redis.ParseURL(redisURL)
	if err != nil {
		t.Fatal(err)
	}
	redisClient := redis.NewClient(redisOptions)
	defer redisClient.Close()
	stream := fmt.Sprintf("ojos:test:problem-projection:%d", suffix)
	defer redisClient.Del(context.Background(), stream)

	snapshot := eventing.ProblemSnapshotData{
		ProblemID:        77,
		AggregateVersion: 1,
		PackageRevision:  1,
		ProblemNo:        "P77",
		Title:            "event integration",
		ProblemType:      "traditional",
		Status:           "published",
		Visibility:       "public",
		CreatedBy:        9,
		TimeLimitMS:      1000,
		MemoryLimitMB:    256,
		PackageArtifact: eventing.ArtifactRef{
			URI:         "storage://problems/package-sha256-test.zip",
			SHA256:      strings.Repeat("a", 64),
			SizeBytes:   321,
			ContentType: "application/zip",
		},
		SourceUpdatedAtUTC: time.Now().UTC(),
	}
	envelope, err := eventing.NewEnvelope(ctx, "ojos://problem-service", eventing.ProblemSnapshotV1, "problem/77", "", 1, snapshot)
	if err != nil {
		t.Fatal(err)
	}
	tx, err := sourceDB.Begin(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if err := eventing.Enqueue(ctx, tx, envelope); err != nil {
		_ = tx.Rollback(ctx)
		t.Fatal(err)
	}
	if err := tx.Commit(ctx); err != nil {
		t.Fatal(err)
	}
	relay := &eventing.Relay{DB: sourceDB, Redis: redisClient, Stream: stream, RelayID: "integration-test"}
	if published, err := relay.PublishBatch(ctx); err != nil || published != 1 {
		t.Fatalf("publish outbox: published=%d err=%v", published, err)
	}

	consumerCtx, stopConsumer := context.WithCancel(ctx)
	consumer := &eventing.Consumer{
		DB:           judgeDB,
		Redis:        redisClient,
		Stream:       stream,
		Group:        "judge-api.problem-projection.v1",
		ConsumerName: "integration-test",
		ClaimIdle:    time.Second,
		MaxAttempts:  1,
		Handler:      ApplyProblemProjection,
	}
	go consumer.Run(consumerCtx)
	defer stopConsumer()

	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		var version int64
		var digest string
		err := judgeDB.QueryRow(ctx, `SELECT aggregate_version, package_artifact_sha256 FROM problems WHERE id = 77`).Scan(&version, &digest)
		if err == nil && version == 1 && digest == snapshot.PackageArtifact.SHA256 {
			var inboxCount int
			if err := judgeDB.QueryRow(ctx, `SELECT COUNT(*) FROM integration_inbox WHERE event_id = $1 AND processed_at IS NOT NULL`, envelope.ID).Scan(&inboxCount); err != nil {
				t.Fatal(err)
			}
			if inboxCount != 1 {
				t.Fatalf("expected one processed inbox row, got %d", inboxCount)
			}
			repo := New(judgeDB)
			submissionID, err := repo.CreateSubmission(ctx, snapshot.ProblemID, 99, "cpp17")
			if err != nil {
				t.Fatal(err)
			}
			submission, err := repo.GetSubmission(ctx, submissionID)
			if err != nil {
				t.Fatal(err)
			}
			if submission.ProblemAggregateVersion != snapshot.AggregateVersion ||
				submission.ProblemPackageRevision != snapshot.PackageRevision ||
				submission.ProblemArtifactURI != snapshot.PackageArtifact.URI ||
				submission.ProblemArtifactSHA256 != snapshot.PackageArtifact.SHA256 ||
				submission.ProblemArtifactSizeBytes != snapshot.PackageArtifact.SizeBytes {
				t.Fatalf("submission did not freeze the problem artifact: %#v", submission)
			}
			exerciseProjectionOrderingDeletionAndDeduplication(
				t,
				ctx,
				sourceDB,
				judgeDB,
				redisClient,
				stream,
				relay,
				envelope,
				snapshot,
				submissionID,
			)
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatal("problem snapshot did not reach judge projection")
}

func exerciseProjectionOrderingDeletionAndDeduplication(
	t *testing.T,
	ctx context.Context,
	sourceDB *pgxpool.Pool,
	judgeDB *pgxpool.Pool,
	redisClient *redis.Client,
	stream string,
	relay *eventing.Relay,
	originalEnvelope eventing.Envelope,
	original eventing.ProblemSnapshotData,
	originalSubmissionID int64,
) {
	t.Helper()
	publish := func(envelope eventing.Envelope) {
		t.Helper()
		tx, err := sourceDB.Begin(ctx)
		if err != nil {
			t.Fatal(err)
		}
		if err := eventing.Enqueue(ctx, tx, envelope); err != nil {
			_ = tx.Rollback(ctx)
			t.Fatal(err)
		}
		if err := tx.Commit(ctx); err != nil {
			t.Fatal(err)
		}
		if published, err := relay.PublishBatch(ctx); err != nil || published != 1 {
			t.Fatalf("publish projection event: published=%d err=%v", published, err)
		}
	}
	waitFor := func(description string, predicate func() bool) {
		t.Helper()
		deadline := time.Now().Add(10 * time.Second)
		for time.Now().Before(deadline) {
			if predicate() {
				return
			}
			time.Sleep(25 * time.Millisecond)
		}
		t.Fatalf("timed out waiting for %s", description)
	}

	newer := original
	newer.AggregateVersion = 3
	newer.PackageRevision = 2
	newer.PackageArtifact.URI = "storage://problems/package-sha256-newer.zip"
	newer.PackageArtifact.SHA256 = strings.Repeat("b", 64)
	newer.SourceUpdatedAtUTC = time.Now().UTC()
	newerEnvelope, err := eventing.NewEnvelope(
		ctx,
		"ojos://problem-service",
		eventing.ProblemSnapshotV1,
		"problem/77",
		"",
		newer.AggregateVersion,
		newer,
	)
	if err != nil {
		t.Fatal(err)
	}
	publish(newerEnvelope)

	older := original
	older.AggregateVersion = 2
	older.PackageRevision = 2
	older.PackageArtifact.URI = "storage://problems/package-sha256-stale.zip"
	older.PackageArtifact.SHA256 = strings.Repeat("c", 64)
	older.SourceUpdatedAtUTC = newer.SourceUpdatedAtUTC.Add(-time.Minute)
	olderEnvelope, err := eventing.NewEnvelope(
		ctx,
		"ojos://problem-service",
		eventing.ProblemSnapshotV1,
		"problem/77",
		"",
		older.AggregateVersion,
		older,
	)
	if err != nil {
		t.Fatal(err)
	}
	publish(olderEnvelope)
	waitFor("newer projection to win over out-of-order delivery", func() bool {
		var version int64
		var digest string
		return judgeDB.QueryRow(ctx, `SELECT aggregate_version, package_artifact_sha256 FROM problems WHERE id = 77`).Scan(&version, &digest) == nil &&
			version == newer.AggregateVersion && digest == newer.PackageArtifact.SHA256
	})

	// Redis delivery is at least once. Re-inserting the original CloudEvent
	// must be acknowledged through the inbox without applying the old snapshot.
	originalPayload, err := json.Marshal(originalEnvelope)
	if err != nil {
		t.Fatal(err)
	}
	if err := redisClient.XAdd(ctx, &redis.XAddArgs{
		Stream: stream,
		Values: map[string]any{"event": string(originalPayload)},
	}).Err(); err != nil {
		t.Fatal(err)
	}
	waitFor("duplicate delivery to be consumed exactly once", func() bool {
		var inboxCount int
		var version int64
		if judgeDB.QueryRow(ctx, `SELECT COUNT(*) FROM integration_inbox WHERE event_id = $1`, originalEnvelope.ID).Scan(&inboxCount) != nil {
			return false
		}
		if judgeDB.QueryRow(ctx, `SELECT aggregate_version FROM problems WHERE id = 77`).Scan(&version) != nil {
			return false
		}
		return inboxCount == 1 && version == newer.AggregateVersion
	})

	deleted := eventing.ProblemDeletedData{ProblemID: original.ProblemID, AggregateVersion: 4}
	deletedEnvelope, err := eventing.NewEnvelope(
		ctx,
		"ojos://problem-service",
		eventing.ProblemDeletedV1,
		"problem/77",
		"",
		deleted.AggregateVersion,
		deleted,
	)
	if err != nil {
		t.Fatal(err)
	}
	publish(deletedEnvelope)
	waitFor("problem tombstone projection", func() bool {
		var version int64
		var tombstoned bool
		return judgeDB.QueryRow(ctx, `SELECT aggregate_version, deleted FROM problems WHERE id = 77`).Scan(&version, &tombstoned) == nil &&
			version == deleted.AggregateVersion && tombstoned
	})

	repo := New(judgeDB)
	if _, err := repo.CreateSubmission(ctx, original.ProblemID, 100, "cpp17"); err == nil {
		t.Fatal("a tombstoned problem must reject new submissions")
	}
	frozen, err := repo.GetSubmission(ctx, originalSubmissionID)
	if err != nil {
		t.Fatal(err)
	}
	if frozen.ProblemAggregateVersion != original.AggregateVersion ||
		frozen.ProblemPackageRevision != original.PackageRevision ||
		frozen.ProblemArtifactSHA256 != original.PackageArtifact.SHA256 {
		t.Fatalf("problem updates/deletion mutated an existing task snapshot: %#v", frozen)
	}

	invalid := original
	invalid.AggregateVersion = 5
	invalid.PackageRevision = 3
	invalid.PackageArtifact.URI = "storage://problems/package-sha256-invalid-contract.zip"
	invalid.PackageArtifact.SHA256 = strings.Repeat("d", 64)
	invalid.SourceUpdatedAtUTC = time.Now().UTC()
	invalidEnvelope, err := eventing.NewEnvelope(
		ctx,
		"ojos://problem-service",
		eventing.ProblemSnapshotV1,
		"problem/77",
		"",
		invalid.AggregateVersion,
		invalid,
	)
	if err != nil {
		t.Fatal(err)
	}
	var invalidData map[string]any
	if err := json.Unmarshal(invalidEnvelope.Data, &invalidData); err != nil {
		t.Fatal(err)
	}
	invalidData["unknown_contract_field"] = true
	invalidEnvelope.Data, _ = json.Marshal(invalidData)
	invalidPayload, err := json.Marshal(invalidEnvelope)
	if err != nil {
		t.Fatal(err)
	}
	if err := redisClient.XAdd(ctx, &redis.XAddArgs{
		Stream: stream,
		Values: map[string]any{"event": string(invalidPayload)},
	}).Err(); err != nil {
		t.Fatal(err)
	}
	waitFor("schema-invalid event to be recorded in the DLQ", func() bool {
		var attempts int
		var lastError string
		if judgeDB.QueryRow(ctx, `
SELECT attempts, last_error
FROM integration_dead_letters
WHERE consumer_name = $1 AND event_id = $2
`, "judge-api.problem-projection.v1", invalidEnvelope.ID).Scan(&attempts, &lastError) != nil {
			return false
		}
		return attempts == 1 && strings.Contains(lastError, "unknown field")
	})
	var invalidInboxCount int
	if err := judgeDB.QueryRow(ctx, `SELECT COUNT(*) FROM integration_inbox WHERE event_id = $1`, invalidEnvelope.ID).Scan(&invalidInboxCount); err != nil {
		t.Fatal(err)
	}
	if invalidInboxCount != 0 {
		t.Fatalf("schema-invalid event entered the projection inbox: count=%d", invalidInboxCount)
	}
}

func poolWithSearchPath(t *testing.T, ctx context.Context, databaseURL, schema string) *pgxpool.Pool {
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
