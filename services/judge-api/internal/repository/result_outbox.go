package repository

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
)

type JudgeResultOutboxRelay struct {
	DB            *pgxpool.Pool
	Redis         redis.UniversalClient
	Stream        string
	RelayID       string
	BatchSize     int
	LeaseDuration time.Duration
	PollInterval  time.Duration
}

type judgeResultOutboxRecord struct {
	Sequence int64
	EventID  string
	Payload  []byte
	Attempts int
}

func (r *JudgeResultOutboxRelay) Run(ctx context.Context) {
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

func (r *JudgeResultOutboxRelay) PublishBatch(ctx context.Context) (int, error) {
	if r == nil || r.DB == nil || r.Redis == nil {
		return 0, errors.New("judge result outbox relay is not configured")
	}
	stream := r.Stream
	if stream == "" {
		stream = "ojos:judge:result"
	}
	relayID := r.RelayID
	if relayID == "" {
		relayID = fmt.Sprintf("judge-result-%d", time.Now().UTC().UnixNano())
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
    FROM judge_result_outbox
    WHERE published_at IS NULL
      AND available_at <= NOW()
      AND (lease_until IS NULL OR lease_until < NOW())
    ORDER BY sequence
    FOR UPDATE SKIP LOCKED
    LIMIT $1
)
UPDATE judge_result_outbox AS outbox
SET lease_owner = $2,
    lease_until = NOW() + $3::interval,
    attempt_count = attempt_count + 1
FROM candidates
WHERE outbox.sequence = candidates.sequence
RETURNING outbox.sequence, outbox.event_id, outbox.payload, outbox.attempt_count
`, batchSize, relayID, durationInterval(lease))
	if err != nil {
		return 0, err
	}
	var records []judgeResultOutboxRecord
	for rows.Next() {
		var record judgeResultOutboxRecord
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
		values := make(map[string]any)
		if err := json.Unmarshal(record.Payload, &values); err != nil {
			r.recordFailure(ctx, relayID, record, err)
			continue
		}
		values["event_id"] = record.EventID
		if _, err := r.Redis.XAdd(ctx, &redis.XAddArgs{
			Stream: stream,
			MaxLen: 10000,
			Approx: true,
			Values: values,
		}).Result(); err != nil {
			r.recordFailure(ctx, relayID, record, err)
			continue
		}
		if _, err := r.DB.Exec(ctx, `
UPDATE judge_result_outbox
SET published_at = NOW(), lease_owner = NULL, lease_until = NULL, last_error = ''
WHERE sequence = $1 AND lease_owner = $2 AND published_at IS NULL
`, record.Sequence, relayID); err != nil {
			// The stream write is at-least-once. A retry may publish a duplicate,
			// but every copy carries the same deterministic event_id.
			continue
		}
		published++
	}
	return published, nil
}

func (r *JudgeResultOutboxRelay) recordFailure(
	ctx context.Context,
	relayID string,
	record judgeResultOutboxRecord,
	publishErr error,
) {
	delay := time.Second
	if record.Attempts >= 2 {
		delay = 5 * time.Second
	}
	if record.Attempts >= 3 {
		delay = 30 * time.Second
	}
	message := publishErr.Error()
	if len(message) > 2048 {
		message = message[:2048]
	}
	_, _ = r.DB.Exec(ctx, `
UPDATE judge_result_outbox
SET lease_owner = NULL,
    lease_until = NULL,
    available_at = NOW() + $3::interval,
    last_error = $4
WHERE sequence = $1 AND lease_owner = $2 AND published_at IS NULL
`, record.Sequence, relayID, durationInterval(delay), message)
}

func durationInterval(duration time.Duration) string {
	return fmt.Sprintf("%d milliseconds", duration.Milliseconds())
}
