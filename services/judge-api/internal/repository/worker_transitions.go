package repository

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
)

func (r *Repository) MarkTaskSucceeded(
	ctx context.Context,
	taskID string,
	workerID string,
	leaseVersion int,
	transition TaskSuccessTransition,
) error {
	if err := validateResultTransition(
		transition.PayloadSHA256,
		transition.OutboxEventID,
		transition.OutboxPayload,
		true,
	); err != nil {
		return err
	}
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return err
	}
	defer tx.Rollback(ctx)

	var transitioned int64
	err = tx.QueryRow(
		ctx,
		`
WITH task AS (
    UPDATE judge_tasks
    SET
        status = 'SUCCEEDED',
        heartbeat_at = NOW(),
        lease_expires_at = NULL,
        error_message = '',
        result_payload_sha256 = $9,
        updated_at = NOW()
    WHERE task_id = $1
      AND worker_id = $2
      AND lease_version = $3
      AND status = 'RUNNING'
      AND lease_expires_at > NOW()
    RETURNING submission_id
), updated_submission AS (
    UPDATE submissions s
    SET
        status = $4,
        score = $5,
        time_ms = $6,
        memory_kb = $7,
        message = $8,
        result_path = $10,
        judged_at = NOW(),
        updated_at = NOW()
    FROM task
    WHERE s.id = task.submission_id
      AND s.status <> 'CANCELLED'
    RETURNING s.id
)
SELECT COUNT(*) FROM task
`,
		taskID,
		workerID,
		leaseVersion,
		transition.Status,
		transition.Score,
		transition.TimeMS,
		transition.MemoryKB,
		transition.Message,
		transition.PayloadSHA256,
		transition.ResultPath,
	).Scan(&transitioned)
	if err != nil {
		return err
	}
	if transitioned == 0 {
		alreadySaved, duplicateErr := taskReportAlreadySaved(
			ctx,
			tx,
			taskID,
			workerID,
			leaseVersion,
			"result",
			transition.PayloadSHA256,
		)
		if duplicateErr != nil {
			return duplicateErr
		}
		if alreadySaved {
			return ErrTaskTransitionAlreadySaved
		}
		return ErrTaskLeaseInvalid
	}
	if err := insertTaskReportReceipt(
		ctx,
		tx,
		taskID,
		workerID,
		leaseVersion,
		"result",
		transition.PayloadSHA256,
		transition.Status,
		transition.OutboxEventID,
	); err != nil {
		return err
	}
	if err := enqueueJudgeResultOutbox(
		ctx,
		tx,
		taskID,
		leaseVersion,
		transition.PayloadSHA256,
		transition.OutboxEventID,
		transition.OutboxPayload,
	); err != nil {
		return err
	}
	return tx.Commit(ctx)
}

func (r *Repository) MarkTaskFailed(
	ctx context.Context,
	taskID string,
	workerID string,
	leaseVersion int,
	transition TaskFailureTransition,
) (TaskFailureOutcome, error) {
	if err := validateResultTransition(
		transition.PayloadSHA256,
		transition.OutboxEventID,
		transition.OutboxPayload,
		true,
	); err != nil {
		return TaskFailureOutcome{}, err
	}
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return TaskFailureOutcome{}, err
	}
	defer tx.Rollback(ctx)

	var attempt int
	err = tx.QueryRow(
		ctx,
		`
SELECT attempt
FROM judge_tasks
WHERE task_id = $1
  AND worker_id = $2
  AND lease_version = $3
  AND status = 'RUNNING'
  AND lease_expires_at > NOW()
FOR UPDATE
`,
		taskID,
		workerID,
		leaseVersion,
	).Scan(&attempt)
	if errors.Is(err, pgx.ErrNoRows) {
		outcome, duplicateErr := taskFailureOutcomeAlreadySaved(
			ctx,
			tx,
			taskID,
			workerID,
			leaseVersion,
			"fail",
			transition.PayloadSHA256,
		)
		if duplicateErr != nil {
			return TaskFailureOutcome{}, duplicateErr
		}
		if outcome != nil {
			return *outcome, ErrTaskTransitionAlreadySaved
		}
		return TaskFailureOutcome{}, ErrTaskLeaseInvalid
	}
	if err != nil {
		return TaskFailureOutcome{}, err
	}

	retryScheduled := transition.Retryable && !taskRetryExhausted(attempt)
	nextTaskStatus := "FAILED"
	nextSubmissionStatus := transition.Status
	responseStatus := transition.Status
	delay := time.Duration(0)
	if retryScheduled {
		nextTaskStatus = "PENDING"
		nextSubmissionStatus = "PENDING"
		responseStatus = "PENDING"
		delay = taskRetryDelay(attempt)
	}
	var submissionID int64
	var availableAt time.Time
	err = tx.QueryRow(ctx, `
UPDATE judge_tasks
SET
    status = $5,
    worker_id = CASE WHEN $6 THEN NULL ELSE worker_id END,
    lease_expires_at = NULL,
    heartbeat_at = NOW(),
    available_at = CASE
        WHEN $6 THEN NOW() + ($7::bigint * interval '1 millisecond')
        ELSE NOW()
    END,
    error_message = $8,
    result_payload_sha256 = $9,
    updated_at = NOW()
WHERE task_id = $1
  AND worker_id = $2
  AND lease_version = $3
  AND attempt = $4
  AND status = 'RUNNING'
  AND lease_expires_at > NOW()
RETURNING submission_id, available_at
`,
		taskID,
		workerID,
		leaseVersion,
		attempt,
		nextTaskStatus,
		retryScheduled,
		delay.Milliseconds(),
		transition.Message,
		transition.PayloadSHA256,
	).Scan(&submissionID, &availableAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return TaskFailureOutcome{}, ErrTaskLeaseInvalid
	}
	if err != nil {
		return TaskFailureOutcome{}, err
	}
	if _, err := tx.Exec(ctx, `
UPDATE submissions
SET
    status = $2,
    score = 0,
    time_ms = 0,
    memory_kb = 0,
    message = $3,
    judged_at = CASE WHEN $2 = 'PENDING' THEN NULL ELSE NOW() END,
    updated_at = NOW()
WHERE id = $1
  AND status <> 'CANCELLED'
`, submissionID, nextSubmissionStatus, transition.Message); err != nil {
		return TaskFailureOutcome{}, err
	}
	if err := insertTaskReportReceipt(
		ctx,
		tx,
		taskID,
		workerID,
		leaseVersion,
		"fail",
		transition.PayloadSHA256,
		responseStatus,
		transition.OutboxEventID,
	); err != nil {
		return TaskFailureOutcome{}, err
	}
	if !retryScheduled {
		if err := enqueueJudgeResultOutbox(
			ctx,
			tx,
			taskID,
			leaseVersion,
			transition.PayloadSHA256,
			transition.OutboxEventID,
			transition.OutboxPayload,
		); err != nil {
			return TaskFailureOutcome{}, err
		}
	}
	if err := tx.Commit(ctx); err != nil {
		return TaskFailureOutcome{}, err
	}
	outcome := TaskFailureOutcome{
		Status:         responseStatus,
		RetryScheduled: retryScheduled,
	}
	if retryScheduled {
		outcome.AvailableAt = &availableAt
	}
	return outcome, nil
}

type taskReportReceipt struct {
	WorkerID       string
	ReportKind     string
	PayloadSHA256  string
	ResponseStatus string
}

func taskFailureOutcomeAlreadySaved(
	ctx context.Context,
	tx pgx.Tx,
	taskID string,
	workerID string,
	leaseVersion int,
	reportKind string,
	payloadSHA256 string,
) (*TaskFailureOutcome, error) {
	var receipt taskReportReceipt
	err := tx.QueryRow(ctx, `
SELECT worker_id, report_kind, payload_sha256, response_status
FROM judge_task_report_receipts
WHERE task_id = $1 AND lease_version = $2
`, taskID, leaseVersion).Scan(
		&receipt.WorkerID,
		&receipt.ReportKind,
		&receipt.PayloadSHA256,
		&receipt.ResponseStatus,
	)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if !matchesTaskReportReceipt(receipt, workerID, reportKind, payloadSHA256) {
		return nil, nil
	}
	return &TaskFailureOutcome{
		Status:         receipt.ResponseStatus,
		RetryScheduled: receipt.ResponseStatus == "PENDING",
		AlreadySaved:   true,
	}, nil
}

func taskReportAlreadySaved(
	ctx context.Context,
	tx pgx.Tx,
	taskID string,
	workerID string,
	leaseVersion int,
	reportKind string,
	payloadSHA256 string,
) (bool, error) {
	var receipt taskReportReceipt
	err := tx.QueryRow(
		ctx,
		`
SELECT
    worker_id,
    report_kind,
    payload_sha256,
    response_status
FROM judge_task_report_receipts
WHERE task_id = $1 AND lease_version = $2
`,
		taskID,
		leaseVersion,
	).Scan(
		&receipt.WorkerID,
		&receipt.ReportKind,
		&receipt.PayloadSHA256,
		&receipt.ResponseStatus,
	)
	if errors.Is(err, pgx.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	return matchesTaskReportReceipt(receipt, workerID, reportKind, payloadSHA256), nil
}

func matchesTaskReportReceipt(
	receipt taskReportReceipt,
	workerID string,
	reportKind string,
	payloadSHA256 string,
) bool {
	return receipt.WorkerID == workerID &&
		receipt.ReportKind == reportKind &&
		receipt.PayloadSHA256 == payloadSHA256
}

func insertTaskReportReceipt(
	ctx context.Context,
	tx pgx.Tx,
	taskID string,
	workerID string,
	leaseVersion int,
	reportKind string,
	payloadSHA256 string,
	responseStatus string,
	eventID string,
) error {
	_, err := tx.Exec(
		ctx,
		`
INSERT INTO judge_task_report_receipts(
    task_id,
    lease_version,
    worker_id,
    report_kind,
    payload_sha256,
    response_status,
    event_id
)
VALUES($1, $2, $3, $4, $5, $6, NULLIF($7, ''))
`,
		taskID,
		leaseVersion,
		workerID,
		reportKind,
		payloadSHA256,
		responseStatus,
		strings.TrimSpace(eventID),
	)
	if err != nil {
		return fmt.Errorf("insert task report receipt: %w", err)
	}
	return nil
}

func validateResultTransition(payloadSHA256 string, eventID string, payload []byte, requiresEvent bool) error {
	if !isLowerHexDigest(payloadSHA256) {
		return errors.New("task result payload sha256 is invalid")
	}
	if !requiresEvent {
		return nil
	}
	if strings.TrimSpace(eventID) == "" || len(payload) == 0 || !json.Valid(payload) {
		return errors.New("terminal task result outbox event is invalid")
	}
	return nil
}

func enqueueJudgeResultOutbox(
	ctx context.Context,
	tx pgx.Tx,
	taskID string,
	leaseVersion int,
	payloadSHA256 string,
	eventID string,
	payload []byte,
) error {
	_, err := tx.Exec(
		ctx,
		`
INSERT INTO judge_result_outbox(
    event_id,
    task_id,
    lease_version,
    payload_sha256,
    payload,
    available_at
)
VALUES($1, $2, $3, $4, $5::jsonb, NOW())
`,
		strings.TrimSpace(eventID),
		taskID,
		leaseVersion,
		payloadSHA256,
		payload,
	)
	if err != nil {
		return fmt.Errorf("enqueue judge result outbox: %w", err)
	}
	return nil
}
