package artifactgc

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

var (
	ErrClaimLost                  = errors.New("artifact GC claim is no longer owned by this collector")
	ErrReferenceIdentityMismatch  = errors.New("artifact GC intent conflicts with the committed Problem reference identity")
	ErrNeedsAttentionNotFound     = errors.New("artifact GC intent is not awaiting operator attention")
	ErrOperatorActorMissing       = errors.New("artifact GC operator actor is required")
	ErrOperatorRetryReasonMissing = errors.New("artifact GC operator retry reason is required")
)

type postgresExecutor interface {
	Exec(context.Context, string, ...any) (pgconn.CommandTag, error)
	Query(context.Context, string, ...any) (pgx.Rows, error)
	QueryRow(context.Context, string, ...any) pgx.Row
}

type PostgresLedger struct {
	DB postgresExecutor
}

func (l PostgresLedger) Claim(ctx context.Context, cutoff time.Time, lease time.Duration) (*Intent, error) {
	if l.DB == nil {
		return nil, errors.New("artifact upload-intent ledger database is required")
	}
	tokenBytes := make([]byte, 24)
	if _, err := rand.Read(tokenBytes); err != nil {
		return nil, err
	}
	token := hex.EncodeToString(tokenBytes)
	var intent Intent
	err := l.DB.QueryRow(ctx, `
WITH candidate AS (
    SELECT artifact_uri
    FROM problem_artifact_upload_intents i
    WHERE i.retry_after <= NOW()
      AND (
          (i.status = 'PENDING' AND (
              i.updated_at <= $1
              OR i.manual_reconcile_requested_at IS NOT NULL
              OR (i.last_operator_retry_at IS NOT NULL AND i.needs_attention_at IS NOT NULL)
          ))
          OR (i.status = 'DELETING' AND i.claim_until <= NOW())
      )
    ORDER BY (i.manual_reconcile_requested_at IS NOT NULL) DESC,
             i.manual_reconcile_requested_at NULLS LAST,
             i.updated_at,
             i.artifact_uri
    FOR UPDATE SKIP LOCKED
    LIMIT 1
)
UPDATE problem_artifact_upload_intents i
SET status = 'DELETING',
    claim_token = $2,
    claim_until = NOW() + make_interval(secs => $3),
    manual_reconcile_requested_at = NULL,
    attempt_count = attempt_count + 1
FROM candidate
WHERE i.artifact_uri = candidate.artifact_uri
RETURNING i.artifact_uri, i.artifact_sha256, i.artifact_size_bytes,
          i.updated_at, i.claim_until, i.attempt_count, i.failure_count
`, cutoff.UTC(), token, lease.Seconds()).Scan(
		&intent.URI, &intent.SHA256, &intent.SizeBytes, &intent.UpdatedAt,
		&intent.ClaimUntil, &intent.AttemptCount, &intent.FailureCount,
	)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	intent.ClaimToken = token
	intent.Key = objectKey(intent.URI)
	return &intent, nil
}

func (l PostgresLedger) ConfirmDeletable(ctx context.Context, intent Intent) (bool, error) {
	var deletable bool
	err := l.DB.QueryRow(ctx, `
SELECT EXISTS (
    SELECT 1
    FROM problem_artifact_upload_intents i
    WHERE i.artifact_uri = $1
      AND i.status = 'DELETING'
      AND i.claim_token = $2
      AND i.claim_until > NOW()
      AND NOT EXISTS (SELECT 1 FROM problems p WHERE p.package_artifact_uri = i.artifact_uri)
      AND NOT EXISTS (SELECT 1 FROM problem_package_revisions r WHERE r.artifact_uri = i.artifact_uri)
      AND NOT EXISTS (SELECT 1 FROM problem_files f WHERE f.storage_path = i.artifact_uri)
)
`, intent.URI, intent.ClaimToken).Scan(&deletable)
	return deletable, err
}

func (l PostgresLedger) Renew(ctx context.Context, intent Intent, lease time.Duration) error {
	tag, err := l.DB.Exec(ctx, `
UPDATE problem_artifact_upload_intents
SET claim_until = NOW() + make_interval(secs => $3)
WHERE artifact_uri = $1
  AND status = 'DELETING'
  AND claim_token = $2
  AND claim_until > NOW()
`, intent.URI, intent.ClaimToken, lease.Seconds())
	if err != nil {
		return err
	}
	if tag.RowsAffected() != 1 {
		return ErrClaimLost
	}
	return nil
}

func (l PostgresLedger) CompleteAbsent(ctx context.Context, intent Intent) error {
	tag, err := l.DB.Exec(ctx, `
DELETE FROM problem_artifact_upload_intents i
WHERE i.artifact_uri = $1 AND i.status = 'DELETING' AND i.claim_token = $2
  AND NOT EXISTS (SELECT 1 FROM problems p WHERE p.package_artifact_uri = i.artifact_uri)
  AND NOT EXISTS (SELECT 1 FROM problem_package_revisions r WHERE r.artifact_uri = i.artifact_uri)
  AND NOT EXISTS (SELECT 1 FROM problem_files f WHERE f.storage_path = i.artifact_uri)
`, intent.URI, intent.ClaimToken)
	if err != nil {
		return err
	}
	if tag.RowsAffected() != 1 {
		return ErrClaimLost
	}
	return nil
}

func (l PostgresLedger) CompleteDeleted(ctx context.Context, intent Intent) error {
	return l.completeOwnedClaim(ctx, intent)
}

func (l PostgresLedger) completeOwnedClaim(ctx context.Context, intent Intent) error {
	tag, err := l.DB.Exec(ctx, `
DELETE FROM problem_artifact_upload_intents
WHERE artifact_uri = $1 AND status = 'DELETING' AND claim_token = $2
`, intent.URI, intent.ClaimToken)
	if err != nil {
		return err
	}
	if tag.RowsAffected() != 1 {
		return ErrClaimLost
	}
	return nil
}

func (l PostgresLedger) CompleteReferenced(ctx context.Context, intent Intent) error {
	tag, err := l.DB.Exec(ctx, `
DELETE FROM problem_artifact_upload_intents i
WHERE i.artifact_uri = $1 AND i.status = 'DELETING' AND i.claim_token = $2
  AND (
      EXISTS (
          SELECT 1 FROM problems p
          WHERE p.package_artifact_uri = i.artifact_uri
            AND LOWER(p.package_artifact_sha256) = i.artifact_sha256
            AND p.package_artifact_size_bytes = i.artifact_size_bytes
      )
      OR EXISTS (
          SELECT 1 FROM problem_package_revisions r
          WHERE r.artifact_uri = i.artifact_uri
            AND LOWER(r.artifact_sha256) = i.artifact_sha256
            AND r.artifact_size_bytes = i.artifact_size_bytes
      )
      OR EXISTS (
          SELECT 1 FROM problem_files f
          WHERE f.storage_path = i.artifact_uri
            AND LOWER(f.sha256) = i.artifact_sha256
            AND f.size_bytes = i.artifact_size_bytes
      )
  )
`, intent.URI, intent.ClaimToken)
	if err != nil {
		return err
	}
	if tag.RowsAffected() != 1 {
		mismatch, queryErr := l.hasOwnedReferenceIdentityMismatch(ctx, intent)
		if queryErr != nil {
			return queryErr
		}
		if mismatch {
			return ErrReferenceIdentityMismatch
		}
		return ErrClaimLost
	}
	return nil
}

func (l PostgresLedger) hasOwnedReferenceIdentityMismatch(ctx context.Context, intent Intent) (bool, error) {
	var owned, referenced, exactReference bool
	err := l.DB.QueryRow(ctx, `
SELECT
    EXISTS (
        SELECT 1 FROM problem_artifact_upload_intents i
        WHERE i.artifact_uri = $1 AND i.status = 'DELETING' AND i.claim_token = $2
    ),
    EXISTS (SELECT 1 FROM problems p WHERE p.package_artifact_uri = $1)
        OR EXISTS (SELECT 1 FROM problem_package_revisions r WHERE r.artifact_uri = $1)
        OR EXISTS (SELECT 1 FROM problem_files f WHERE f.storage_path = $1),
    EXISTS (
        SELECT 1 FROM problem_artifact_upload_intents i
        WHERE i.artifact_uri = $1 AND i.status = 'DELETING' AND i.claim_token = $2
          AND (
              EXISTS (
                  SELECT 1 FROM problems p
                  WHERE p.package_artifact_uri = i.artifact_uri
                    AND LOWER(p.package_artifact_sha256) = i.artifact_sha256
                    AND p.package_artifact_size_bytes = i.artifact_size_bytes
              )
              OR EXISTS (
                  SELECT 1 FROM problem_package_revisions r
                  WHERE r.artifact_uri = i.artifact_uri
                    AND LOWER(r.artifact_sha256) = i.artifact_sha256
                    AND r.artifact_size_bytes = i.artifact_size_bytes
              )
              OR EXISTS (
                  SELECT 1 FROM problem_files f
                  WHERE f.storage_path = i.artifact_uri
                    AND LOWER(f.sha256) = i.artifact_sha256
                    AND f.size_bytes = i.artifact_size_bytes
              )
          )
    )
`, intent.URI, intent.ClaimToken).Scan(&owned, &referenced, &exactReference)
	return owned && referenced && !exactReference, err
}

// Release returns a dry-run claim to PENDING without recording a failure. No
// provider mutation has started on this path, so retaining delete isolation is
// unnecessary and would only block a legitimate publisher.
func (l PostgresLedger) Release(ctx context.Context, intent Intent, delay time.Duration) error {
	tag, err := l.DB.Exec(ctx, `
UPDATE problem_artifact_upload_intents
SET status = 'PENDING',
    retry_after = NOW() + make_interval(secs => $3),
    claim_token = NULL,
    claim_until = NULL
WHERE artifact_uri = $1 AND status = 'DELETING' AND claim_token = $2
`, intent.URI, intent.ClaimToken, delay.Seconds())
	if err != nil {
		return err
	}
	if tag.RowsAffected() != 1 {
		return ErrClaimLost
	}
	return nil
}

func (l PostgresLedger) Retry(ctx context.Context, intent Intent, failure FailureDetail, delay time.Duration) error {
	failure = boundedFailureDetail(failure)
	tag, err := l.DB.Exec(ctx, `
UPDATE problem_artifact_upload_intents
SET retry_after = NOW() + make_interval(secs => $3),
    last_error = $4,
    last_failure_stage = $5,
    last_failure_kind = $6,
    last_failure_http_status = $7,
    last_failure_provider_result = $8,
    last_failure_deterministic = $9,
    failure_count = failure_count + 1
WHERE artifact_uri = $1 AND status = 'DELETING' AND claim_token = $2
`, intent.URI, intent.ClaimToken, delay.Seconds(), failure.Message,
		failure.Stage, failure.Kind, failure.HTTPStatus, failure.ProviderResult, failure.Deterministic)
	if err != nil {
		return err
	}
	if tag.RowsAffected() != 1 {
		return ErrClaimLost
	}
	return nil
}

// Quarantine transfers an owned claim to operator control. It is token-CAS so
// a stale collector cannot quarantine a newer attempt or a re-published URI.
func (l PostgresLedger) Quarantine(ctx context.Context, intent Intent, failure FailureDetail) error {
	failure = boundedFailureDetail(failure)
	tag, err := l.DB.Exec(ctx, `
UPDATE problem_artifact_upload_intents
SET status = 'NEEDS_ATTENTION',
    retry_after = NOW(),
    claim_token = NULL,
    claim_until = NULL,
    manual_reconcile_requested_at = NULL,
    failure_count = failure_count + 1,
    last_error = $3,
    last_failure_stage = $4,
    last_failure_kind = $5,
    last_failure_http_status = $6,
    last_failure_provider_result = $7,
    last_failure_deterministic = $8,
    needs_attention_at = NOW()
WHERE artifact_uri = $1 AND status = 'DELETING' AND claim_token = $2
`, intent.URI, intent.ClaimToken, failure.Message, failure.Stage, failure.Kind,
		failure.HTTPStatus, failure.ProviderResult, failure.Deterministic)
	if err != nil {
		return err
	}
	if tag.RowsAffected() != 1 {
		return ErrClaimLost
	}
	return nil
}

func boundedLedgerMessage(message string) string {
	message = strings.TrimSpace(message)
	runes := []rune(message)
	if len(runes) > 2000 {
		message = string(runes[:2000])
	}
	return message
}

func boundedLedgerActor(actor string) string {
	actor = strings.TrimSpace(actor)
	runes := []rune(actor)
	if len(runes) > 255 {
		actor = string(runes[:255])
	}
	return actor
}

func objectKey(uri string) string {
	trimmed := strings.TrimPrefix(strings.TrimSpace(uri), "storage://")
	if slash := strings.IndexByte(trimmed, '/'); slash >= 0 {
		return trimmed[slash+1:]
	}
	return ""
}
