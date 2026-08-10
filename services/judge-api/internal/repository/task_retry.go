package repository

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
)

const taskMaximumAttempts = 4

type TaskFailureOutcome struct {
	Status         string
	RetryScheduled bool
	AvailableAt    *time.Time
	AlreadySaved   bool
}

func taskRetryExhausted(attempt int) bool {
	return attempt >= taskMaximumAttempts
}

func taskRetryDelay(attempt int) time.Duration {
	switch {
	case attempt <= 1:
		return time.Second
	case attempt == 2:
		return 5 * time.Second
	default:
		return 30 * time.Second
	}
}

func taskRetryAvailableAt(now time.Time, attempt int) time.Time {
	return now.Add(taskRetryDelay(attempt))
}

type expiredTaskLease struct {
	TaskID         string
	SubmissionID   int64
	WorkerID       string
	LeaseVersion   int
	Attempt        int
	LeaseExpiresAt time.Time
}

func expiredTaskFailureTransition(task expiredTaskLease) (TaskFailureTransition, error) {
	const message = "task retry budget exhausted after lease expiry"
	payload, err := json.Marshal(map[string]string{
		"created_at":    task.LeaseExpiresAt.UTC().Format(time.RFC3339Nano),
		"lease_version": strconv.Itoa(task.LeaseVersion),
		"memory_kb":     "0",
		"message":       message,
		"producer":      "judge-api-service",
		"score":         "0",
		"status":        "SYSTEM_ERROR",
		"submission_id": strconv.FormatInt(task.SubmissionID, 10),
		"task_id":       task.TaskID,
		"time_ms":       "0",
		"type":          "judge.result.submitted",
		"worker_id":     task.WorkerID,
	})
	if err != nil {
		return TaskFailureTransition{}, fmt.Errorf("marshal exhausted task result: %w", err)
	}
	payloadDigest := sha256.Sum256(payload)
	payloadSHA256 := hex.EncodeToString(payloadDigest[:])
	eventDigest := sha256.Sum256([]byte(
		"judge-result\x00" + strings.TrimSpace(task.TaskID) + "\x00" +
			strconv.Itoa(task.LeaseVersion) + "\x00" + payloadSHA256,
	))
	return TaskFailureTransition{
		Status:        "SYSTEM_ERROR",
		Message:       message,
		Retryable:     false,
		PayloadSHA256: payloadSHA256,
		OutboxEventID: "judge-result-" + hex.EncodeToString(eventDigest[:]),
		OutboxPayload: payload,
	}, nil
}

// RecoverStaleTasks moves expired leases through the same bounded retry budget
// as an explicit retryable Worker failure. Rows are locked with SKIP LOCKED so
// concurrent claim requests can help recovery without transitioning one lease
// twice. Exhaustion commits the task/submission state, receipt and result outbox
// in one PostgreSQL transaction.
func (r *Repository) RecoverStaleTasks(ctx context.Context) (int64, error) {
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return 0, err
	}
	defer func() {
		_ = tx.Rollback(ctx)
	}()

	rows, err := tx.Query(ctx, `
SELECT
    task_id,
    submission_id,
    COALESCE(worker_id, ''),
    lease_version,
    attempt,
    lease_expires_at
FROM judge_tasks
WHERE status = 'RUNNING'
  AND lease_expires_at < NOW()
ORDER BY id
FOR UPDATE SKIP LOCKED
`)
	if err != nil {
		return 0, err
	}
	stale := make([]expiredTaskLease, 0)
	for rows.Next() {
		var task expiredTaskLease
		if err := rows.Scan(
			&task.TaskID,
			&task.SubmissionID,
			&task.WorkerID,
			&task.LeaseVersion,
			&task.Attempt,
			&task.LeaseExpiresAt,
		); err != nil {
			rows.Close()
			return 0, err
		}
		stale = append(stale, task)
	}
	if err := rows.Err(); err != nil {
		rows.Close()
		return 0, err
	}
	rows.Close()

	var recovered int64
	for _, task := range stale {
		if taskRetryExhausted(task.Attempt) {
			transition, err := expiredTaskFailureTransition(task)
			if err != nil {
				return 0, err
			}
			changed, err := exhaustExpiredTask(ctx, tx, task, transition)
			if err != nil {
				return 0, err
			}
			if changed {
				recovered++
			}
			continue
		}
		changed, err := scheduleExpiredTaskRetry(ctx, tx, task)
		if err != nil {
			return 0, err
		}
		if changed {
			recovered++
		}
	}

	if err := tx.Commit(ctx); err != nil {
		return 0, err
	}
	return recovered, nil
}

func scheduleExpiredTaskRetry(
	ctx context.Context,
	tx pgx.Tx,
	task expiredTaskLease,
) (bool, error) {
	tag, err := tx.Exec(ctx, `
UPDATE judge_tasks
SET
    status = 'PENDING',
    worker_id = NULL,
    lease_expires_at = NULL,
    heartbeat_at = NOW(),
    available_at = NOW() + ($5::bigint * interval '1 millisecond'),
    error_message = 'lease expired',
    updated_at = NOW()
WHERE task_id = $1
  AND COALESCE(worker_id, '') = $2
  AND lease_version = $3
  AND attempt = $4
  AND status = 'RUNNING'
  AND lease_expires_at < NOW()
`, task.TaskID, task.WorkerID, task.LeaseVersion, task.Attempt, taskRetryDelay(task.Attempt).Milliseconds())
	if err != nil {
		return false, err
	}
	if tag.RowsAffected() == 0 {
		return false, nil
	}
	if _, err := tx.Exec(ctx, `
UPDATE submissions
SET status = 'PENDING',
    message = 'lease expired; retry scheduled',
    judged_at = NULL,
    updated_at = NOW()
WHERE id = $1
  AND status = 'JUDGING'
`, task.SubmissionID); err != nil {
		return false, err
	}
	return true, nil
}

func exhaustExpiredTask(
	ctx context.Context,
	tx pgx.Tx,
	task expiredTaskLease,
	transition TaskFailureTransition,
) (bool, error) {
	tag, err := tx.Exec(ctx, `
UPDATE judge_tasks
SET
    status = 'FAILED',
    lease_expires_at = NULL,
    heartbeat_at = NOW(),
    available_at = NOW(),
    error_message = $5,
    result_payload_sha256 = $6,
    updated_at = NOW()
WHERE task_id = $1
  AND COALESCE(worker_id, '') = $2
  AND lease_version = $3
  AND attempt = $4
  AND status = 'RUNNING'
  AND lease_expires_at < NOW()
`, task.TaskID, task.WorkerID, task.LeaseVersion, task.Attempt, transition.Message, transition.PayloadSHA256)
	if err != nil {
		return false, err
	}
	if tag.RowsAffected() == 0 {
		return false, nil
	}
	if _, err := tx.Exec(ctx, `
UPDATE submissions
SET status = 'SYSTEM_ERROR',
    score = 0,
    time_ms = 0,
    memory_kb = 0,
    message = $2,
    judged_at = NOW(),
    updated_at = NOW()
WHERE id = $1
  AND status <> 'CANCELLED'
`, task.SubmissionID, transition.Message); err != nil {
		return false, err
	}
	if err := insertTaskReportReceipt(
		ctx,
		tx,
		task.TaskID,
		task.WorkerID,
		task.LeaseVersion,
		"fail",
		transition.PayloadSHA256,
		transition.Status,
		transition.OutboxEventID,
	); err != nil {
		return false, err
	}
	if err := enqueueJudgeResultOutbox(
		ctx,
		tx,
		task.TaskID,
		task.LeaseVersion,
		transition.PayloadSHA256,
		transition.OutboxEventID,
		transition.OutboxPayload,
	); err != nil {
		return false, err
	}
	return true, nil
}
