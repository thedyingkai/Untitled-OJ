package eventing

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
)

type SQLExecutor interface {
	Exec(context.Context, string, ...any) (pgconn.CommandTag, error)
}

// TypedEvent is an envelope that has already passed its domain codec. Keeping
// this wrapper opaque prevents business publishers from bypassing type/schema
// and payload validation before writing the durable outbox.
type TypedEvent struct {
	envelope Envelope
}

func (event TypedEvent) Envelope() Envelope { return event.envelope }

// Enqueue writes an event through the caller's transaction. Passing a pool is
// supported for repair tools, but mutation paths must pass their active pgx.Tx.
func Enqueue(ctx context.Context, db SQLExecutor, event TypedEvent) error {
	if db == nil {
		return errors.New("outbox database is not configured")
	}
	envelope := event.envelope
	if envelope.Type == "" {
		return errors.New("outbox event was not produced by a codec")
	}
	if err := envelope.Validate(); err != nil {
		return err
	}
	payload, err := json.Marshal(envelope)
	if err != nil {
		return fmt.Errorf("marshal outbox envelope: %w", err)
	}
	_, err = db.Exec(ctx, `
INSERT INTO integration_outbox(
    event_id,
    aggregate_type,
    aggregate_id,
    aggregate_version,
    event_type,
    payload,
    occurred_at,
    available_at
)
VALUES($1, $2, $3, $4, $5, $6::jsonb, $7, NOW())
ON CONFLICT(event_id) DO NOTHING
`, envelope.ID, aggregateType(envelope.Subject), envelope.Subject, envelope.AggregateVersion, envelope.Type, payload, envelope.Time)
	return err
}

type Relay struct {
	DB            *pgxpool.Pool
	Redis         redis.UniversalClient
	RelayID       string
	BatchSize     int
	LeaseDuration time.Duration
	PollInterval  time.Duration
	streamName    string
}

func NewRelay(db *pgxpool.Pool, redisClient redis.UniversalClient, transport TransportConfig) (*Relay, error) {
	if transport.stream == "" {
		return nil, errors.New("outbox relay transport stream is required")
	}
	if db == nil || redisClient == nil {
		return nil, errors.New("outbox relay dependencies are not configured")
	}
	return &Relay{DB: db, Redis: redisClient, streamName: transport.stream}, nil
}

type outboxRecord struct {
	Sequence int64
	EventID  string
	Payload  []byte
	Attempts int
}

// ReplayPublished makes the retained source outbox authoritative again after
// a broker or consumer-database disaster. Reusing the original event IDs is
// intentional: an intact inbox treats the replay as a no-op, while a rebuilt
// inbox can reconstruct every projection including tombstones.
func (r *Relay) ReplayPublished(ctx context.Context) (int64, error) {
	if r.DB == nil {
		return 0, errors.New("outbox relay database is not configured")
	}
	tag, err := r.DB.Exec(ctx, `
UPDATE integration_outbox
SET published_at = NULL,
    available_at = NOW(),
    lease_owner = NULL,
    lease_until = NULL,
    last_error = ''
WHERE published_at IS NOT NULL
`)
	return tag.RowsAffected(), err
}

func (r *Relay) Run(ctx context.Context) {
	interval := r.PollInterval
	if interval <= 0 {
		interval = 250 * time.Millisecond
	}
	for {
		if ctx.Err() != nil {
			return
		}
		published, _ := r.PublishBatch(ctx)
		if published > 0 {
			continue
		}
		timer := time.NewTimer(interval)
		select {
		case <-ctx.Done():
			timer.Stop()
			return
		case <-timer.C:
		}
	}
}

func (r *Relay) PublishBatch(ctx context.Context) (int, error) {
	if r.DB == nil || r.Redis == nil {
		return 0, errors.New("outbox relay dependencies are not configured")
	}
	stream := r.streamName
	if stream == "" {
		stream = DefaultEventStream
	}
	relayID := r.RelayID
	if relayID == "" {
		relayID = newEventID()
	}
	batchSize := r.BatchSize
	if batchSize <= 0 || batchSize > 500 {
		batchSize = 100
	}
	lease := r.LeaseDuration
	if lease <= 0 {
		lease = 30 * time.Second
	}

	rows, err := r.DB.Query(ctx, `
WITH candidates AS (
    SELECT sequence
    FROM integration_outbox
    WHERE published_at IS NULL
      AND available_at <= NOW()
      AND (lease_until IS NULL OR lease_until < NOW())
    ORDER BY sequence
    FOR UPDATE SKIP LOCKED
    LIMIT $1
)
UPDATE integration_outbox AS outbox
SET lease_owner = $2,
    lease_until = NOW() + $3::interval,
    attempt_count = attempt_count + 1
FROM candidates
WHERE outbox.sequence = candidates.sequence
RETURNING outbox.sequence, outbox.event_id, outbox.payload, outbox.attempt_count
`, batchSize, relayID, intervalLiteral(lease))
	if err != nil {
		return 0, err
	}
	var records []outboxRecord
	for rows.Next() {
		var record outboxRecord
		if err := rows.Scan(&record.Sequence, &record.EventID, &record.Payload, &record.Attempts); err != nil {
			rows.Close()
			return 0, err
		}
		records = append(records, record)
	}
	if err := rows.Err(); err != nil {
		rows.Close()
		return 0, err
	}
	rows.Close()

	published := 0
	for _, record := range records {
		if ctx.Err() != nil {
			return published, ctx.Err()
		}
		envelope, err := DecodeEnvelopeStrict(record.Payload)
		if err != nil {
			r.recordFailure(ctx, relayID, record, err)
			continue
		}
		_, err = r.Redis.XAdd(ctx, &redis.XAddArgs{
			Stream: stream,
			Values: map[string]any{
				"event_id": envelope.ID,
				"type":     envelope.Type,
				"subject":  envelope.Subject,
				"event":    string(record.Payload),
			},
		}).Result()
		if err != nil {
			r.recordFailure(ctx, relayID, record, err)
			continue
		}
		if _, err := r.DB.Exec(ctx, `
UPDATE integration_outbox
SET published_at = NOW(), lease_owner = NULL, lease_until = NULL, last_error = ''
WHERE sequence = $1 AND lease_owner = $2 AND published_at IS NULL
`, record.Sequence, relayID); err != nil {
			// A retry may publish a duplicate. Consumers are required to dedupe by
			// event_id, so losing the acknowledgement is safe.
			continue
		}
		published++
	}
	return published, nil
}

func (r *Relay) recordFailure(ctx context.Context, relayID string, record outboxRecord, publishErr error) {
	delay := time.Second
	if record.Attempts >= 2 {
		delay = 5 * time.Second
	}
	if record.Attempts >= 3 {
		delay = 30 * time.Second
	}
	_, _ = r.DB.Exec(ctx, `
UPDATE integration_outbox
SET lease_owner = NULL,
    lease_until = NULL,
    available_at = NOW() + $3::interval,
    last_error = $4
WHERE sequence = $1 AND lease_owner = $2 AND published_at IS NULL
`, record.Sequence, relayID, intervalLiteral(delay), truncateError(publishErr))
}

func aggregateType(subject string) string {
	// The outbox aggregate is a transport/indexing concern. Derive it from the
	// generic CloudEvents subject instead of maintaining an event-type registry.
	subject = strings.TrimSpace(subject)
	if separator := strings.IndexByte(subject, '/'); separator > 0 {
		candidate := subject[:separator]
		for _, char := range candidate {
			if (char < 'a' || char > 'z') && (char < '0' || char > '9') && char != '-' && char != '_' {
				return "integration"
			}
		}
		return candidate
	}
	return "integration"
}

func intervalLiteral(duration time.Duration) string {
	return fmt.Sprintf("%d milliseconds", duration.Milliseconds())
}

func truncateError(err error) string {
	if err == nil {
		return ""
	}
	value := err.Error()
	if len(value) > 2048 {
		return value[:2048]
	}
	return value
}
