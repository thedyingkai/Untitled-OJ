package repository

import (
	"context"
	"errors"
	"strconv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
)

var (
	ErrWorkerNotFound   = errors.New("worker not found")
	ErrWorkerDraining   = errors.New("worker is draining")
	ErrTaskLeaseInvalid = errors.New("task lease is invalid")
	ErrTaskNotFound     = errors.New("task not found")
)

type WorkerRegistration struct {
	WorkerID           string
	WorkerName         string
	Hostname           string
	Version            string
	Capabilities       []string
	SupportedLanguages []string
	MaxConcurrency     int
}

type WorkerView struct {
	WorkerID           string
	WorkerName         string
	Hostname           string
	Version            string
	Capabilities       []string
	SupportedLanguages []string
	MaxConcurrency     int
	RunningCount       int
	Status             string
	Drain              bool
	LastSeen           time.Time
	RegisteredAt       time.Time
	UpdatedAt          time.Time
}

type TaskLeaseView struct {
	TaskID         string
	SubmissionID   int64
	ProblemID      int64
	Language       string
	WorkerID       string
	LeaseVersion   int
	LeaseExpiresAt time.Time
	HeartbeatAt    time.Time
	Attempt        int
	Status         string
}

type QueueTaskCounts struct {
	Scheduled int64
	Pending   int64
	Judging   int64
}

func (r *Repository) UpsertWorker(ctx context.Context, w WorkerRegistration) (*WorkerView, error) {
	if w.MaxConcurrency <= 0 {
		w.MaxConcurrency = 1
	}

	var view WorkerView
	err := r.db.QueryRow(
		ctx,
		`
INSERT INTO judge_workers(
    worker_id,
    worker_name,
    hostname,
    version,
    capabilities,
    supported_languages,
    max_concurrency,
    running_count,
    status,
    drain,
    last_seen,
    registered_at,
    updated_at
)
VALUES($1, $2, $3, $4, jsonb_build_object('items', $5::text[]), $6, $7, 0, 'ONLINE', FALSE, NOW(), NOW(), NOW())
ON CONFLICT(worker_id) DO UPDATE
SET
    worker_name = EXCLUDED.worker_name,
    hostname = EXCLUDED.hostname,
    version = EXCLUDED.version,
    capabilities = EXCLUDED.capabilities,
    supported_languages = EXCLUDED.supported_languages,
    max_concurrency = EXCLUDED.max_concurrency,
    status = CASE WHEN judge_workers.drain THEN 'DRAINING' ELSE 'ONLINE' END,
    last_seen = NOW(),
    updated_at = NOW()
RETURNING
    worker_id,
    worker_name,
    hostname,
    version,
    COALESCE(ARRAY(SELECT jsonb_array_elements_text(capabilities->'items')), '{}'::text[]),
    supported_languages,
    max_concurrency,
    running_count,
    status,
    drain,
    last_seen,
    registered_at,
    updated_at
`,
		w.WorkerID,
		w.WorkerName,
		w.Hostname,
		w.Version,
		w.Capabilities,
		w.SupportedLanguages,
		w.MaxConcurrency,
	).Scan(
		&view.WorkerID,
		&view.WorkerName,
		&view.Hostname,
		&view.Version,
		&view.Capabilities,
		&view.SupportedLanguages,
		&view.MaxConcurrency,
		&view.RunningCount,
		&view.Status,
		&view.Drain,
		&view.LastSeen,
		&view.RegisteredAt,
		&view.UpdatedAt,
	)

	return &view, err
}

func (r *Repository) WorkerHeartbeat(ctx context.Context, workerID string, runningCount int) (*WorkerView, error) {
	var view WorkerView
	err := r.db.QueryRow(
		ctx,
		`
UPDATE judge_workers
SET
    running_count = $2,
    status = CASE WHEN drain THEN 'DRAINING' ELSE 'ONLINE' END,
    last_seen = NOW(),
    updated_at = NOW()
WHERE worker_id = $1
RETURNING
    worker_id,
    worker_name,
    hostname,
    version,
    COALESCE(ARRAY(SELECT jsonb_array_elements_text(capabilities->'items')), '{}'::text[]),
    supported_languages,
    max_concurrency,
    running_count,
    status,
    drain,
    last_seen,
    registered_at,
    updated_at
`,
		workerID,
		runningCount,
	).Scan(
		&view.WorkerID,
		&view.WorkerName,
		&view.Hostname,
		&view.Version,
		&view.Capabilities,
		&view.SupportedLanguages,
		&view.MaxConcurrency,
		&view.RunningCount,
		&view.Status,
		&view.Drain,
		&view.LastSeen,
		&view.RegisteredAt,
		&view.UpdatedAt,
	)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, ErrWorkerNotFound
	}
	return &view, err
}

func (r *Repository) EnsureTaskForSubmission(ctx context.Context, submissionID int64) error {
	taskID := deterministicTaskID(submissionID)
	_, err := r.db.Exec(
		ctx,
		`
INSERT INTO judge_tasks(task_id, submission_id, problem_id, language, status, created_at, updated_at)
SELECT $2, s.id, s.problem_id, s.language, 'PENDING', NOW(), NOW()
FROM submissions s
WHERE s.id = $1
ON CONFLICT(submission_id) DO UPDATE
SET
    problem_id = EXCLUDED.problem_id,
    language = EXCLUDED.language,
    worker_id = NULL,
    lease_expires_at = NULL,
    heartbeat_at = NULL,
    status = 'PENDING',
    error_message = '',
    updated_at = NOW()
`,
		submissionID,
		taskID,
	)
	return err
}

func (r *Repository) RecoverStaleTasks(ctx context.Context) (int64, error) {
	tag, err := r.db.Exec(
		ctx,
		`
WITH stale AS (
    UPDATE judge_tasks
    SET
        status = 'PENDING',
        worker_id = NULL,
        lease_expires_at = NULL,
        heartbeat_at = NOW(),
        error_message = 'lease expired',
        updated_at = NOW()
    WHERE status = 'RUNNING'
      AND lease_expires_at < NOW()
    RETURNING submission_id
)
UPDATE submissions s
SET status = 'PENDING',
    updated_at = NOW()
FROM stale
WHERE s.id = stale.submission_id
  AND s.status = 'JUDGING'
`,
	)
	if err != nil {
		return 0, err
	}
	return tag.RowsAffected(), nil
}

func (r *Repository) ClaimTasks(
	ctx context.Context,
	workerID string,
	supportedLanguages []string,
	limit int,
	leaseTTL time.Duration,
	taskIDs []string,
) ([]TaskLeaseView, error) {
	if limit <= 0 {
		return []TaskLeaseView{}, nil
	}
	if limit > 16 {
		limit = 16
	}
	if leaseTTL <= 0 {
		leaseTTL = time.Minute
	}

	tx, err := r.db.Begin(ctx)
	if err != nil {
		return nil, err
	}
	defer func() {
		_ = tx.Rollback(ctx)
	}()

	var drain bool
	if err := tx.QueryRow(ctx, `SELECT drain FROM judge_workers WHERE worker_id = $1`, workerID).Scan(&drain); err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return nil, ErrWorkerNotFound
		}
		return nil, err
	}
	if drain {
		return nil, ErrWorkerDraining
	}

	rows, err := tx.Query(
		ctx,
		`
WITH inserted AS (
    INSERT INTO judge_tasks(task_id, submission_id, problem_id, language, status, created_at, updated_at)
    SELECT ('sub-' || s.id::text), s.id, s.problem_id, s.language, 'PENDING', NOW(), NOW()
    FROM submissions s
    WHERE s.status = 'PENDING'
    ON CONFLICT(submission_id) DO NOTHING
    RETURNING id
),
candidate AS (
    SELECT jt.id
    FROM judge_tasks jt
    JOIN submissions s ON s.id = jt.submission_id
    WHERE jt.status = 'PENDING'
      AND s.status = 'PENDING'
      AND (cardinality($2::text[]) = 0 OR jt.language = ANY($2::text[]))
      AND (cardinality($5::text[]) = 0 OR jt.task_id = ANY($5::text[]))
    ORDER BY jt.id ASC
    FOR UPDATE SKIP LOCKED
    LIMIT $3
),
updated_tasks AS (
    UPDATE judge_tasks jt
    SET
        worker_id = $1,
        lease_version = jt.lease_version + 1,
        lease_expires_at = NOW() + ($4::bigint * interval '1 second'),
        heartbeat_at = NOW(),
        attempt = jt.attempt + 1,
        status = 'RUNNING',
        error_message = '',
        updated_at = NOW()
    FROM candidate
    WHERE jt.id = candidate.id
    RETURNING
        jt.task_id,
        jt.submission_id,
        jt.problem_id,
        jt.language,
        jt.worker_id,
        jt.lease_version,
        jt.lease_expires_at,
        jt.heartbeat_at,
        jt.attempt,
        jt.status
),
claimed_submissions AS (
    UPDATE submissions s
    SET status = 'JUDGING',
        updated_at = NOW()
    FROM updated_tasks ut
    WHERE s.id = ut.submission_id
      AND s.status = 'PENDING'
    RETURNING s.id
)
SELECT
    ut.task_id,
    ut.submission_id,
    ut.problem_id,
    ut.language,
    ut.worker_id,
    ut.lease_version,
    ut.lease_expires_at,
    ut.heartbeat_at,
    ut.attempt,
    ut.status
FROM updated_tasks ut
JOIN claimed_submissions cs ON cs.id = ut.submission_id
ORDER BY ut.submission_id ASC
`,
		workerID,
		supportedLanguages,
		limit,
		int64(leaseTTL.Seconds()),
		normalizeTaskIDs(taskIDs),
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	leases := make([]TaskLeaseView, 0)
	for rows.Next() {
		var lease TaskLeaseView
		if err := rows.Scan(
			&lease.TaskID,
			&lease.SubmissionID,
			&lease.ProblemID,
			&lease.Language,
			&lease.WorkerID,
			&lease.LeaseVersion,
			&lease.LeaseExpiresAt,
			&lease.HeartbeatAt,
			&lease.Attempt,
			&lease.Status,
		); err != nil {
			return nil, err
		}
		leases = append(leases, lease)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	if err := tx.Commit(ctx); err != nil {
		return nil, err
	}

	return leases, nil
}

func (r *Repository) RefreshTaskLease(
	ctx context.Context,
	taskID string,
	workerID string,
	leaseVersion int,
	leaseTTL time.Duration,
) (*TaskLeaseView, error) {
	if leaseTTL <= 0 {
		leaseTTL = time.Minute
	}

	var lease TaskLeaseView
	err := r.db.QueryRow(
		ctx,
		`
UPDATE judge_tasks
SET
    lease_expires_at = NOW() + ($4::bigint * interval '1 second'),
    heartbeat_at = NOW(),
    updated_at = NOW()
WHERE task_id = $1
  AND worker_id = $2
  AND lease_version = $3
  AND status = 'RUNNING'
  AND lease_expires_at > NOW()
RETURNING
    task_id,
    submission_id,
    problem_id,
    language,
    worker_id,
    lease_version,
    lease_expires_at,
    heartbeat_at,
    attempt,
    status
`,
		taskID,
		workerID,
		leaseVersion,
		int64(leaseTTL.Seconds()),
	).Scan(
		&lease.TaskID,
		&lease.SubmissionID,
		&lease.ProblemID,
		&lease.Language,
		&lease.WorkerID,
		&lease.LeaseVersion,
		&lease.LeaseExpiresAt,
		&lease.HeartbeatAt,
		&lease.Attempt,
		&lease.Status,
	)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, ErrTaskLeaseInvalid
	}
	return &lease, err
}

func (r *Repository) GetTaskForLease(
	ctx context.Context,
	taskID string,
	workerID string,
	leaseVersion int,
) (*TaskLeaseView, error) {
	var lease TaskLeaseView
	err := r.db.QueryRow(
		ctx,
		`
SELECT
    task_id,
    submission_id,
    problem_id,
    language,
    COALESCE(worker_id, ''),
    lease_version,
    COALESCE(lease_expires_at, NOW()),
    COALESCE(heartbeat_at, NOW()),
    attempt,
    status
FROM judge_tasks
WHERE task_id = $1
  AND worker_id = $2
  AND lease_version = $3
`,
		taskID,
		workerID,
		leaseVersion,
	).Scan(
		&lease.TaskID,
		&lease.SubmissionID,
		&lease.ProblemID,
		&lease.Language,
		&lease.WorkerID,
		&lease.LeaseVersion,
		&lease.LeaseExpiresAt,
		&lease.HeartbeatAt,
		&lease.Attempt,
		&lease.Status,
	)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, ErrTaskLeaseInvalid
	}
	return &lease, err
}

func (r *Repository) MarkTaskSucceeded(
	ctx context.Context,
	taskID string,
	workerID string,
	leaseVersion int,
	status string,
	score int,
	timeMS int,
	memoryKB int,
	message string,
) error {
	tag, err := r.db.Exec(
		ctx,
		`
WITH task AS (
    UPDATE judge_tasks
    SET
        status = 'SUCCEEDED',
        heartbeat_at = NOW(),
        lease_expires_at = NULL,
        error_message = '',
        updated_at = NOW()
    WHERE task_id = $1
      AND worker_id = $2
      AND lease_version = $3
      AND status = 'RUNNING'
    RETURNING submission_id
)
UPDATE submissions s
SET
    status = $4,
    score = $5,
    time_ms = $6,
    memory_kb = $7,
    message = $8,
    judged_at = NOW(),
    updated_at = NOW()
FROM task
WHERE s.id = task.submission_id
  AND s.status <> 'CANCELLED'
`,
		taskID,
		workerID,
		leaseVersion,
		status,
		score,
		timeMS,
		memoryKB,
		message,
	)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return ErrTaskLeaseInvalid
	}
	return nil
}

func (r *Repository) MarkTaskFailed(
	ctx context.Context,
	taskID string,
	workerID string,
	leaseVersion int,
	status string,
	message string,
	retryable bool,
) error {
	nextTaskStatus := "FAILED"
	nextSubmissionStatus := status
	if retryable {
		nextTaskStatus = "PENDING"
		nextSubmissionStatus = "PENDING"
	}

	tag, err := r.db.Exec(
		ctx,
		`
WITH task AS (
    UPDATE judge_tasks
    SET
        status = $4,
        worker_id = CASE WHEN $7 THEN NULL ELSE worker_id END,
        lease_expires_at = NULL,
        heartbeat_at = NOW(),
        error_message = $5,
        updated_at = NOW()
    WHERE task_id = $1
      AND worker_id = $2
      AND lease_version = $3
      AND status = 'RUNNING'
    RETURNING submission_id
)
UPDATE submissions s
SET
    status = $6,
    score = 0,
    time_ms = 0,
    memory_kb = 0,
    message = $5,
    judged_at = CASE WHEN $6 = 'PENDING' THEN NULL ELSE NOW() END,
    updated_at = NOW()
FROM task
WHERE s.id = task.submission_id
  AND s.status <> 'CANCELLED'
`,
		taskID,
		workerID,
		leaseVersion,
		nextTaskStatus,
		message,
		nextSubmissionStatus,
		retryable,
	)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return ErrTaskLeaseInvalid
	}
	return nil
}

func (r *Repository) ListWorkers(ctx context.Context, offlineAfter time.Duration) ([]WorkerView, error) {
	rows, err := r.db.Query(
		ctx,
		`
UPDATE judge_workers
SET status = 'OFFLINE',
    updated_at = NOW()
WHERE last_seen < NOW() - ($1::bigint * interval '1 second')
  AND status <> 'OFFLINE'
  AND drain = FALSE
RETURNING worker_id
`,
		int64(offlineAfter.Seconds()),
	)
	if err != nil {
		return nil, err
	}
	rows.Close()

	rows, err = r.db.Query(
		ctx,
		`
SELECT
    worker_id,
    worker_name,
    hostname,
    version,
    COALESCE(ARRAY(SELECT jsonb_array_elements_text(capabilities->'items')), '{}'::text[]),
    supported_languages,
    max_concurrency,
    running_count,
    status,
    drain,
    last_seen,
    registered_at,
    updated_at
FROM judge_workers
ORDER BY last_seen DESC, worker_id ASC
`,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	workers := make([]WorkerView, 0)
	for rows.Next() {
		var view WorkerView
		if err := rows.Scan(
			&view.WorkerID,
			&view.WorkerName,
			&view.Hostname,
			&view.Version,
			&view.Capabilities,
			&view.SupportedLanguages,
			&view.MaxConcurrency,
			&view.RunningCount,
			&view.Status,
			&view.Drain,
			&view.LastSeen,
			&view.RegisteredAt,
			&view.UpdatedAt,
		); err != nil {
			return nil, err
		}
		workers = append(workers, view)
	}
	return workers, rows.Err()
}

func (r *Repository) ListTasks(ctx context.Context, limit int) ([]TaskLeaseView, error) {
	if limit <= 0 {
		limit = 100
	}
	if limit > 500 {
		limit = 500
	}

	rows, err := r.db.Query(
		ctx,
		`
SELECT
    task_id,
    submission_id,
    problem_id,
    language,
    COALESCE(worker_id, ''),
    lease_version,
    COALESCE(lease_expires_at, NOW()),
    COALESCE(heartbeat_at, NOW()),
    attempt,
    status
FROM judge_tasks
ORDER BY updated_at DESC, id DESC
LIMIT $1
`,
		limit,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	tasks := make([]TaskLeaseView, 0)
	for rows.Next() {
		var task TaskLeaseView
		if err := rows.Scan(
			&task.TaskID,
			&task.SubmissionID,
			&task.ProblemID,
			&task.Language,
			&task.WorkerID,
			&task.LeaseVersion,
			&task.LeaseExpiresAt,
			&task.HeartbeatAt,
			&task.Attempt,
			&task.Status,
		); err != nil {
			return nil, err
		}
		tasks = append(tasks, task)
	}
	return tasks, rows.Err()
}

func (r *Repository) QueueTaskCounts(ctx context.Context) (*QueueTaskCounts, error) {
	var counts QueueTaskCounts
	err := r.db.QueryRow(
		ctx,
		`
SELECT
    COUNT(*) FILTER (WHERE status = 'PENDING') AS scheduled,
    COUNT(*) FILTER (WHERE status = 'PENDING') AS pending,
    COUNT(*) FILTER (WHERE status = 'RUNNING') AS judging
FROM judge_tasks
`,
	).Scan(&counts.Scheduled, &counts.Pending, &counts.Judging)
	return &counts, err
}

func (r *Repository) DrainWorker(ctx context.Context, workerID string) error {
	tag, err := r.db.Exec(
		ctx,
		`
UPDATE judge_workers
SET drain = TRUE,
    status = 'DRAINING',
    updated_at = NOW()
WHERE worker_id = $1
`,
		workerID,
	)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return ErrWorkerNotFound
	}
	return nil
}

func (r *Repository) RequeueSubmission(ctx context.Context, submissionID int64) error {
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return err
	}
	defer func() {
		_ = tx.Rollback(ctx)
	}()

	tag, err := tx.Exec(
		ctx,
		`
UPDATE submissions
SET status = 'PENDING',
    score = 0,
    time_ms = 0,
    memory_kb = 0,
    message = '',
    judged_at = NULL,
    cancelled_at = NULL,
    cancel_reason = '',
    updated_at = NOW()
WHERE id = $1
`,
		submissionID,
	)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return ErrSubmissionNotFound
	}

	_, err = tx.Exec(
		ctx,
		`
INSERT INTO judge_tasks(task_id, submission_id, problem_id, language, status, created_at, updated_at)
SELECT $2, s.id, s.problem_id, s.language, 'PENDING', NOW(), NOW()
FROM submissions s
WHERE s.id = $1
ON CONFLICT(submission_id) DO UPDATE
SET
    worker_id = NULL,
    lease_expires_at = NULL,
    heartbeat_at = NULL,
    status = 'PENDING',
    error_message = '',
    updated_at = NOW()
`,
		submissionID,
		deterministicTaskID(submissionID),
	)
	if err != nil {
		return err
	}

	return tx.Commit(ctx)
}

func deterministicTaskID(submissionID int64) string {
	return "sub-" + strconv.FormatInt(submissionID, 10)
}

func normalizeTaskIDs(taskIDs []string) []string {
	seen := make(map[string]struct{}, len(taskIDs))
	normalized := make([]string, 0, len(taskIDs))
	for _, taskID := range taskIDs {
		taskID = strings.TrimSpace(taskID)
		if taskID == "" {
			continue
		}
		if _, ok := seen[taskID]; ok {
			continue
		}
		seen[taskID] = struct{}{}
		normalized = append(normalized, taskID)
	}
	return normalized
}
