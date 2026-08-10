package artifactgc

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

const (
	FailureKindTransient                 = "TRANSIENT"
	FailureKindProviderHTTP              = "PROVIDER_HTTP"
	FailureKindObjectIdentityMismatch    = "OBJECT_IDENTITY_MISMATCH"
	FailureKindReferenceIdentityMismatch = "REFERENCE_IDENTITY_MISMATCH"
	FailureKindReferencedObjectMissing   = "REFERENCED_OBJECT_MISSING"
	FailureKindLedger                    = "LEDGER"
	FailureKindDeterministic             = "DETERMINISTIC"

	operatorActionRetry     = "RETRY"
	operatorActionReconcile = "RECONCILE"
	operatorSchemaVersion   = 2
	maxOperatorPageSize     = 200
)

var (
	ErrOperatorStatusInvalid       = errors.New("artifact GC status must be PENDING, DELETING, or NEEDS_ATTENTION")
	ErrOperatorIdentityInvalid     = errors.New("artifact GC operator identity is invalid")
	ErrOperatorIdempotencyMissing  = errors.New("Idempotency-Key is required")
	ErrOperatorIdempotencyConflict = errors.New("conflict: Idempotency-Key was already used for a different artifact GC request")
	ErrOperatorStateConflict       = errors.New("conflict: artifact GC intent state or identity changed")
)

// FailureDetail is persisted separately from the bounded diagnostic message,
// so operator tooling never needs to parse provider error text. ProviderResult
// is a classification (for example HTTP_404), never a response body or token.
type FailureDetail struct {
	Message        string
	Stage          string
	Kind           string
	HTTPStatus     *int
	ProviderResult string
	Deterministic  bool
}

type IntentRecord struct {
	URI                        string
	SHA256                     string
	SizeBytes                  int64
	Status                     string
	FailureCount               int
	LastError                  string
	LastFailureStage           string
	LastFailureKind            string
	LastFailureHTTPStatus      *int
	LastFailureProviderResult  string
	LastFailureDeterministic   bool
	UploadCompletedAt          *time.Time
	NeedsAttentionAt           *time.Time
	ManualReconcileRequestedAt *time.Time
	LastOperatorRetryReason    string
	LastOperatorRetryAt        *time.Time
	UpdatedAt                  time.Time
}

type IntentPage struct {
	Items      []IntentRecord
	NextCursor string
}

type OperatorActionResult struct {
	ActionID   int64
	Replayed   bool
	FromStatus string
	ToStatus   string
}

// RecoveryDue is a lightweight indexed existence probe used between full
// retention sweeps. It recovers expired claims and durable manual requests
// without turning the configured 24-hour orphan scan interval into a poll.
func (l PostgresLedger) RecoveryDue(ctx context.Context) (bool, error) {
	if l.DB == nil {
		return false, errors.New("artifact upload-intent ledger database is required")
	}
	var due bool
	err := l.DB.QueryRow(ctx, `
SELECT EXISTS (
    SELECT 1
    FROM problem_artifact_upload_intents
    WHERE retry_after <= NOW()
      AND (
          (status = 'PENDING' AND (
              manual_reconcile_requested_at IS NOT NULL
              OR (last_operator_retry_at IS NOT NULL AND needs_attention_at IS NOT NULL)
          ))
          OR (status = 'DELETING' AND claim_until <= NOW())
      )
)
`).Scan(&due)
	return due, err
}

func (l PostgresLedger) ListIntents(ctx context.Context, status, cursor string, limit int) (IntentPage, error) {
	status = strings.ToUpper(strings.TrimSpace(status))
	if !validOperatorStatus(status) {
		return IntentPage{}, ErrOperatorStatusInvalid
	}
	cursor = strings.TrimSpace(cursor)
	if limit <= 0 {
		limit = 100
	}
	if limit > maxOperatorPageSize {
		return IntentPage{}, fmt.Errorf("artifact GC list limit must not exceed %d", maxOperatorPageSize)
	}
	if l.DB == nil {
		return IntentPage{}, errors.New("artifact upload-intent ledger database is required")
	}
	rows, err := l.DB.Query(ctx, `
SELECT artifact_uri, artifact_sha256, artifact_size_bytes, status,
       failure_count, last_error,
       last_failure_stage, last_failure_kind, last_failure_http_status,
       last_failure_provider_result, last_failure_deterministic,
       upload_completed_at, needs_attention_at, manual_reconcile_requested_at,
       last_operator_retry_reason, last_operator_retry_at, updated_at
FROM problem_artifact_upload_intents
WHERE status = $1 AND artifact_uri > $2
ORDER BY artifact_uri
LIMIT $3
`, status, cursor, limit+1)
	if err != nil {
		return IntentPage{}, err
	}
	defer rows.Close()

	items := make([]IntentRecord, 0, limit)
	for rows.Next() {
		var item IntentRecord
		if err := rows.Scan(
			&item.URI, &item.SHA256, &item.SizeBytes, &item.Status,
			&item.FailureCount, &item.LastError,
			&item.LastFailureStage, &item.LastFailureKind, &item.LastFailureHTTPStatus,
			&item.LastFailureProviderResult, &item.LastFailureDeterministic,
			&item.UploadCompletedAt, &item.NeedsAttentionAt, &item.ManualReconcileRequestedAt,
			&item.LastOperatorRetryReason, &item.LastOperatorRetryAt, &item.UpdatedAt,
		); err != nil {
			return IntentPage{}, err
		}
		items = append(items, item)
	}
	if err := rows.Err(); err != nil {
		return IntentPage{}, err
	}

	page := IntentPage{Items: items}
	if len(page.Items) > limit {
		page.NextCursor = page.Items[limit-1].URI
		page.Items = page.Items[:limit]
	}
	return page, nil
}

func (l PostgresLedger) RequestReconcile(
	ctx context.Context,
	uri string,
	digest string,
	sizeBytes int64,
	actor string,
	reason string,
	idempotencyKey string,
) (OperatorActionResult, error) {
	uri, digest, actor, reason, idempotencyKey, err := normalizeOperatorRequest(uri, digest, sizeBytes, actor, reason, idempotencyKey)
	if err != nil {
		return OperatorActionResult{}, err
	}
	requestHash := hashOperatorRequest(operatorActionReconcile, uri, digest, sizeBytes, 0, actor, reason)
	if existing, found, err := l.lookupOperatorAction(ctx, idempotencyKey, requestHash); err != nil || found {
		return existing, err
	}

	var result OperatorActionResult
	err = l.DB.QueryRow(ctx, `
WITH previous AS MATERIALIZED (
    SELECT artifact_uri, artifact_sha256, artifact_size_bytes, status,
           failure_count, last_error, needs_attention_at,
           last_failure_stage, last_failure_kind, last_failure_http_status,
           last_failure_provider_result, last_failure_deterministic
    FROM problem_artifact_upload_intents
    WHERE artifact_uri = $1
      AND artifact_sha256 = $2
      AND artifact_size_bytes = $3
      AND status = 'PENDING'
      AND upload_completed_at IS NOT NULL
    FOR UPDATE
), requested AS (
    UPDATE problem_artifact_upload_intents i
    SET manual_reconcile_requested_at = NOW(), retry_after = NOW()
    FROM previous p
    WHERE i.artifact_uri = p.artifact_uri
    RETURNING i.artifact_uri
)
INSERT INTO problem_artifact_gc_operator_actions(
    action_schema_version, artifact_uri, action, actor, reason,
    previous_status, previous_failure_count, previous_last_error,
    previous_needs_attention_at, idempotency_key, request_hash,
    artifact_sha256, artifact_size_bytes, from_status, to_status,
    previous_last_failure_stage, previous_last_failure_kind,
    previous_last_failure_http_status, previous_last_failure_provider_result,
    previous_last_failure_deterministic
)
SELECT $8, p.artifact_uri, 'RECONCILE', $4, $5,
       p.status, p.failure_count, p.last_error, p.needs_attention_at,
       $6, $7, p.artifact_sha256, p.artifact_size_bytes, 'PENDING', 'PENDING',
       p.last_failure_stage, p.last_failure_kind, p.last_failure_http_status,
       p.last_failure_provider_result, p.last_failure_deterministic
FROM previous p
JOIN requested r ON r.artifact_uri = p.artifact_uri
RETURNING action_id, from_status, to_status
`, uri, digest, sizeBytes, actor, reason, idempotencyKey, requestHash, operatorSchemaVersion).Scan(
		&result.ActionID, &result.FromStatus, &result.ToStatus,
	)
	if err == nil {
		return result, nil
	}
	return l.resolveOperatorMutationError(ctx, err, idempotencyKey, requestHash)
}

// RetryNeedsAttention is an optimistic, idempotent operator transition. It
// preserves the forensic failure snapshot, resets the automatic failure budget
// and marks completed uploads for the targeted collector lane.
func (l PostgresLedger) RetryNeedsAttention(
	ctx context.Context,
	uri string,
	expectedFailureCount int,
	actor string,
	reason string,
	idempotencyKey string,
) (OperatorActionResult, error) {
	uri = strings.TrimSpace(uri)
	actor = boundedLedgerActor(actor)
	reason = boundedLedgerMessage(reason)
	idempotencyKey = strings.TrimSpace(idempotencyKey)
	if actor == "" {
		return OperatorActionResult{}, ErrOperatorActorMissing
	}
	if uri == "" || reason == "" {
		return OperatorActionResult{}, ErrOperatorRetryReasonMissing
	}
	if expectedFailureCount < 1 {
		return OperatorActionResult{}, ErrOperatorStateConflict
	}
	if err := validateIdempotencyKey(idempotencyKey); err != nil {
		return OperatorActionResult{}, err
	}
	requestHash := hashOperatorRequest(operatorActionRetry, uri, "", 0, expectedFailureCount, actor, reason)
	if existing, found, err := l.lookupOperatorAction(ctx, idempotencyKey, requestHash); err != nil || found {
		return existing, err
	}

	var result OperatorActionResult
	err := l.DB.QueryRow(ctx, `
WITH previous AS MATERIALIZED (
    SELECT artifact_uri, artifact_sha256, artifact_size_bytes, status,
           failure_count, last_error, needs_attention_at,
           last_failure_stage, last_failure_kind, last_failure_http_status,
           last_failure_provider_result, last_failure_deterministic,
           upload_completed_at
    FROM problem_artifact_upload_intents
    WHERE artifact_uri = $1
      AND status = 'NEEDS_ATTENTION'
      AND failure_count = $2
    FOR UPDATE
), recovered AS (
    UPDATE problem_artifact_upload_intents i
    SET status = 'PENDING',
        retry_after = NOW(),
        claim_token = NULL,
        claim_until = NULL,
        failure_count = 0,
        manual_reconcile_requested_at = CASE
            WHEN p.upload_completed_at IS NOT NULL THEN NOW()
            ELSE NULL
        END,
        last_operator_retry_reason = $4,
        last_operator_retry_at = NOW()
    FROM previous p
    WHERE i.artifact_uri = p.artifact_uri
    RETURNING i.artifact_uri
)
INSERT INTO problem_artifact_gc_operator_actions(
    action_schema_version, artifact_uri, action, actor, reason,
    previous_status, previous_failure_count, previous_last_error,
    previous_needs_attention_at, idempotency_key, request_hash,
    artifact_sha256, artifact_size_bytes, from_status, to_status,
    previous_last_failure_stage, previous_last_failure_kind,
    previous_last_failure_http_status, previous_last_failure_provider_result,
    previous_last_failure_deterministic
)
SELECT $7, p.artifact_uri, 'RETRY', $3, $4,
       p.status, p.failure_count, p.last_error, p.needs_attention_at,
       $5, $6, p.artifact_sha256, p.artifact_size_bytes,
       'NEEDS_ATTENTION', 'PENDING',
       p.last_failure_stage, p.last_failure_kind, p.last_failure_http_status,
       p.last_failure_provider_result, p.last_failure_deterministic
FROM previous p
JOIN recovered r ON r.artifact_uri = p.artifact_uri
RETURNING action_id, from_status, to_status
`, uri, expectedFailureCount, actor, reason, idempotencyKey, requestHash, operatorSchemaVersion).Scan(
		&result.ActionID, &result.FromStatus, &result.ToStatus,
	)
	if err == nil {
		return result, nil
	}
	return l.resolveOperatorMutationError(ctx, err, idempotencyKey, requestHash)
}

func (l PostgresLedger) lookupOperatorAction(
	ctx context.Context,
	idempotencyKey string,
	requestHash string,
) (OperatorActionResult, bool, error) {
	if l.DB == nil {
		return OperatorActionResult{}, false, errors.New("artifact upload-intent ledger database is required")
	}
	var result OperatorActionResult
	var existingHash string
	err := l.DB.QueryRow(ctx, `
SELECT action_id, request_hash, from_status, to_status
FROM problem_artifact_gc_operator_actions
WHERE idempotency_key = $1
`, idempotencyKey).Scan(&result.ActionID, &existingHash, &result.FromStatus, &result.ToStatus)
	if errors.Is(err, pgx.ErrNoRows) {
		return OperatorActionResult{}, false, nil
	}
	if err != nil {
		return OperatorActionResult{}, false, err
	}
	if existingHash != requestHash {
		return OperatorActionResult{}, true, ErrOperatorIdempotencyConflict
	}
	result.Replayed = true
	return result, true, nil
}

func (l PostgresLedger) resolveOperatorMutationError(
	ctx context.Context,
	mutationErr error,
	idempotencyKey string,
	requestHash string,
) (OperatorActionResult, error) {
	// Always re-read the idempotency ledger after a failed mutation. A
	// concurrent identical request may have committed while this statement was
	// waiting on the intent row lock, in which case PostgreSQL legitimately
	// returns no row rather than a unique violation.
	result, found, lookupErr := l.lookupOperatorAction(ctx, idempotencyKey, requestHash)
	if lookupErr != nil {
		return OperatorActionResult{}, lookupErr
	}
	if found {
		return result, nil
	}
	var postgresErr *pgconn.PgError
	if errors.As(mutationErr, &postgresErr) && postgresErr.Code == "23505" {
		return OperatorActionResult{}, ErrOperatorIdempotencyConflict
	}
	if errors.Is(mutationErr, pgx.ErrNoRows) {
		return OperatorActionResult{}, ErrOperatorStateConflict
	}
	return OperatorActionResult{}, mutationErr
}

func normalizeOperatorRequest(
	uri string,
	digest string,
	sizeBytes int64,
	actor string,
	reason string,
	idempotencyKey string,
) (string, string, string, string, string, error) {
	uri = strings.TrimSpace(uri)
	digest = strings.TrimPrefix(strings.ToLower(strings.TrimSpace(digest)), "sha256:")
	actor = boundedLedgerActor(actor)
	reason = boundedLedgerMessage(reason)
	idempotencyKey = strings.TrimSpace(idempotencyKey)
	if actor == "" {
		return "", "", "", "", "", ErrOperatorActorMissing
	}
	if reason == "" {
		return "", "", "", "", "", ErrOperatorRetryReasonMissing
	}
	decoded, err := hex.DecodeString(digest)
	if !strings.HasPrefix(uri, "storage://") || err != nil || len(decoded) != sha256.Size || sizeBytes < 0 {
		return "", "", "", "", "", ErrOperatorIdentityInvalid
	}
	if err := validateIdempotencyKey(idempotencyKey); err != nil {
		return "", "", "", "", "", err
	}
	return uri, digest, actor, reason, idempotencyKey, nil
}

func validateIdempotencyKey(value string) error {
	if value == "" {
		return ErrOperatorIdempotencyMissing
	}
	if len(value) > 255 {
		return errors.New("Idempotency-Key must not exceed 255 bytes")
	}
	for _, character := range value {
		if character < 0x21 || character > 0x7e {
			return errors.New("Idempotency-Key must contain visible ASCII characters only")
		}
	}
	return nil
}

func hashOperatorRequest(action, uri, digest string, sizeBytes int64, expectedFailureCount int, actor, reason string) string {
	payload, _ := json.Marshal(struct {
		Action               string `json:"action"`
		URI                  string `json:"uri"`
		SHA256               string `json:"sha256"`
		SizeBytes            int64  `json:"size_bytes"`
		ExpectedFailureCount int    `json:"expected_failure_count"`
		Actor                string `json:"actor"`
		Reason               string `json:"reason"`
	}{action, uri, digest, sizeBytes, expectedFailureCount, actor, reason})
	sum := sha256.Sum256(payload)
	return hex.EncodeToString(sum[:])
}

func validOperatorStatus(status string) bool {
	switch status {
	case "PENDING", "DELETING", "NEEDS_ATTENTION":
		return true
	default:
		return false
	}
}

func classifyFailure(stage string, cause error, deterministic bool) FailureDetail {
	stage = strings.TrimSpace(stage)
	detail := FailureDetail{Stage: stage, Deterministic: deterministic}
	var providerErr *ProviderHTTPError
	switch {
	case errors.As(cause, &providerErr):
		status := providerErr.StatusCode
		detail.Kind = FailureKindProviderHTTP
		detail.HTTPStatus = &status
		detail.ProviderResult = fmt.Sprintf("HTTP_%d", status)
		detail.Message = fmt.Sprintf("%s failed with provider HTTP %d", stage, status)
		detail.Deterministic = providerErr.Deterministic() || deterministic
	case errors.Is(cause, ErrReferenceIdentityMismatch):
		detail.Kind = FailureKindReferenceIdentityMismatch
		detail.Message = "committed reference identity does not match the artifact intent"
		detail.Deterministic = true
	case stage == "identity":
		detail.Kind = FailureKindObjectIdentityMismatch
		detail.Message = "stored object identity does not match the artifact intent"
		detail.Deterministic = true
	case stage == "referenced object missing":
		detail.Kind = FailureKindReferencedObjectMissing
		detail.Message = "a committed reference exists but the bound object is missing"
		detail.Deterministic = true
	case strings.Contains(stage, "ledger") || strings.Contains(stage, "claim") ||
		strings.Contains(stage, "reference") || strings.Contains(stage, "complete") ||
		strings.Contains(stage, "release") || strings.Contains(stage, "renew"):
		detail.Kind = FailureKindLedger
		detail.Message = stage + " failed"
	case deterministic:
		detail.Kind = FailureKindDeterministic
		detail.Message = stage + " failed deterministically"
	default:
		detail.Kind = FailureKindTransient
		detail.Message = stage + " failed transiently"
	}
	return boundedFailureDetail(detail)
}

func boundedFailureDetail(detail FailureDetail) FailureDetail {
	detail.Message = boundedLedgerMessage(detail.Message)
	detail.Stage = boundedText(detail.Stage, 255)
	detail.ProviderResult = boundedText(detail.ProviderResult, 255)
	if !validFailureKind(detail.Kind) {
		if detail.Deterministic {
			detail.Kind = FailureKindDeterministic
		} else {
			detail.Kind = FailureKindTransient
		}
	}
	if detail.HTTPStatus != nil && (*detail.HTTPStatus < 100 || *detail.HTTPStatus > 599) {
		detail.HTTPStatus = nil
	}
	return detail
}

func boundedText(value string, limit int) string {
	value = strings.TrimSpace(value)
	runes := []rune(value)
	if len(runes) > limit {
		return string(runes[:limit])
	}
	return value
}

func validFailureKind(kind string) bool {
	switch kind {
	case FailureKindTransient, FailureKindProviderHTTP,
		FailureKindObjectIdentityMismatch, FailureKindReferenceIdentityMismatch,
		FailureKindReferencedObjectMissing, FailureKindLedger,
		FailureKindDeterministic:
		return true
	default:
		return false
	}
}
