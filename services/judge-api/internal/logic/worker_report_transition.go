package logic

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
	"time"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"go.uber.org/zap"
)

func successTransition(
	taskID string,
	workerID string,
	req *types.WorkerSubmitResultReq,
) (repository.TaskSuccessTransition, error) {
	digest, err := reportPayloadSHA256(req)
	if err != nil {
		return repository.TaskSuccessTransition{}, err
	}
	eventID := judgeResultEventID(taskID, req.LeaseVersion, digest)
	payload, err := json.Marshal(judgeResultEventValues(taskID, workerID, req, time.Now().UTC()))
	if err != nil {
		return repository.TaskSuccessTransition{}, fmt.Errorf("marshal judge result outbox payload: %w", err)
	}
	return repository.TaskSuccessTransition{
		Status:        req.Status,
		Score:         req.Score,
		TimeMS:        req.TimeMs,
		MemoryKB:      req.MemoryKb,
		Message:       req.Message,
		PayloadSHA256: digest,
		OutboxEventID: eventID,
		OutboxPayload: payload,
	}, nil
}

func failureTransition(
	taskID string,
	workerID string,
	errorType string,
	retryable bool,
	result *types.WorkerSubmitResultReq,
) (repository.TaskFailureTransition, error) {
	canonical := struct {
		TaskID       string                       `json:"task_id"`
		WorkerID     string                       `json:"worker_id"`
		LeaseVersion int                          `json:"lease_version"`
		ErrorType    string                       `json:"error_type"`
		Retryable    bool                         `json:"retryable"`
		Result       *types.WorkerSubmitResultReq `json:"result"`
	}{
		TaskID:       taskID,
		WorkerID:     workerID,
		LeaseVersion: result.LeaseVersion,
		ErrorType:    errorType,
		Retryable:    retryable,
		Result:       result,
	}
	digest, err := reportPayloadSHA256(canonical)
	if err != nil {
		return repository.TaskFailureTransition{}, err
	}
	transition := repository.TaskFailureTransition{
		Status:        result.Status,
		Message:       result.Message,
		Retryable:     retryable,
		PayloadSHA256: digest,
	}
	// A retryable request can become the terminal fourth attempt inside the
	// repository. Build the deterministic terminal event up front; the first
	// three attempts keep it out of the outbox, while exhaustion commits it in
	// the same transaction as the effective SYSTEM_ERROR outcome.
	transition.OutboxEventID = judgeResultEventID(taskID, result.LeaseVersion, digest)
	transition.OutboxPayload, err = json.Marshal(
		judgeResultEventValues(taskID, workerID, result, time.Now().UTC()),
	)
	if err != nil {
		return repository.TaskFailureTransition{}, fmt.Errorf("marshal judge failure outbox payload: %w", err)
	}
	return transition, nil
}

func reportPayloadSHA256(payload any) (string, error) {
	encoded, err := json.Marshal(payload)
	if err != nil {
		return "", fmt.Errorf("marshal worker report payload: %w", err)
	}
	digest := sha256.Sum256(encoded)
	return hex.EncodeToString(digest[:]), nil
}

func judgeResultEventID(taskID string, leaseVersion int, payloadSHA256 string) string {
	digest := sha256.Sum256([]byte(
		"judge-result\x00" + strings.TrimSpace(taskID) + "\x00" +
			strconv.Itoa(leaseVersion) + "\x00" + payloadSHA256,
	))
	return "judge-result-" + hex.EncodeToString(digest[:])
}

func flushCommittedJudgeResult(
	ctx context.Context,
	svcCtx *svc.ServiceContext,
	taskID string,
	workerID string,
	req *types.WorkerSubmitResultReq,
	alreadySaved bool,
) error {
	if svcCtx.ResultOutbox != nil {
		if _, err := svcCtx.ResultOutbox.PublishBatch(ctx); err != nil && svcCtx.Logger != nil {
			// The PG outbox is authoritative; a Redis outage must not turn an
			// acknowledged task transition into an ambiguous Worker retry.
			svcCtx.Logger.Warn(
				"judge result outbox flush deferred",
				zap.String("task_id", taskID),
				zap.Error(err),
			)
		}
		return nil
	}
	// Lightweight logic/handler fixtures do not construct a PostgreSQL-backed
	// ServiceContext. Preserve their direct stream observation without creating
	// a second production delivery path.
	if alreadySaved || svcCtx.Redis == nil {
		return nil
	}
	return publishJudgeResultEvent(ctx, svcCtx, taskID, workerID, req)
}
