package eventing

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
)

type ProjectionHandler func(context.Context, pgx.Tx, Envelope) error

type Consumer struct {
	DB           *pgxpool.Pool
	Redis        redis.UniversalClient
	Stream       string
	Group        string
	ConsumerName string
	BatchSize    int64
	ClaimIdle    time.Duration
	MaxAttempts  int
	Handler      ProjectionHandler
}

func (c *Consumer) Run(ctx context.Context) {
	if err := c.ensureGroup(ctx); err != nil {
		if ctx.Err() != nil {
			return
		}
	}
	for {
		if ctx.Err() != nil {
			return
		}
		messages, err := c.read(ctx)
		if err != nil {
			timer := time.NewTimer(time.Second)
			select {
			case <-ctx.Done():
				timer.Stop()
				return
			case <-timer.C:
			}
			continue
		}
		for _, message := range messages {
			_ = c.process(ctx, message)
		}
	}
}

func (c *Consumer) ensureGroup(ctx context.Context) error {
	if c.Redis == nil {
		return errors.New("projection Redis client is not configured")
	}
	err := c.Redis.XGroupCreateMkStream(ctx, c.stream(), c.group(), "0").Err()
	if err == nil || redis.HasErrorPrefix(err, "BUSYGROUP") {
		return nil
	}
	return err
}

func (c *Consumer) read(ctx context.Context) ([]redis.XMessage, error) {
	if err := c.ensureGroup(ctx); err != nil {
		return nil, err
	}
	claimIdle := c.ClaimIdle
	if claimIdle <= 0 {
		claimIdle = 30 * time.Second
	}
	count := c.BatchSize
	if count <= 0 || count > 500 {
		count = 100
	}
	claimed, _, err := c.Redis.XAutoClaim(ctx, &redis.XAutoClaimArgs{
		Stream:   c.stream(),
		Group:    c.group(),
		Consumer: c.consumerName(),
		MinIdle:  claimIdle,
		Start:    "0-0",
		Count:    count,
	}).Result()
	if err == nil && len(claimed) > 0 {
		return claimed, nil
	}
	streams, err := c.Redis.XReadGroup(ctx, &redis.XReadGroupArgs{
		Group:    c.group(),
		Consumer: c.consumerName(),
		Streams:  []string{c.stream(), ">"},
		Count:    count,
		Block:    time.Second,
	}).Result()
	if err != nil {
		if errors.Is(err, redis.Nil) {
			return nil, nil
		}
		return nil, err
	}
	var messages []redis.XMessage
	for _, stream := range streams {
		messages = append(messages, stream.Messages...)
	}
	return messages, nil
}

func (c *Consumer) process(ctx context.Context, message redis.XMessage) error {
	raw, ok := message.Values["event"]
	if !ok {
		return c.fail(ctx, message, "", errors.New("Redis integration event has no event field"))
	}
	payload := fmt.Sprint(raw)
	envelope, err := DecodeEnvelopeStrict([]byte(payload))
	if err != nil {
		return c.fail(ctx, message, envelope.ID, err)
	}
	if c.DB == nil || c.Handler == nil {
		return errors.New("projection consumer dependencies are not configured")
	}

	tx, err := c.DB.Begin(ctx)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	tag, err := tx.Exec(ctx, `
INSERT INTO integration_inbox(consumer_name, event_id, event_type, received_at)
VALUES($1, $2, $3, NOW())
ON CONFLICT(consumer_name, event_id) DO NOTHING
`, c.group(), envelope.ID, envelope.Type)
	if err != nil {
		return err
	}
	if tag.RowsAffected() > 0 {
		if err := c.Handler(ctx, tx, envelope); err != nil {
			return c.fail(ctx, message, envelope.ID, err)
		}
		if _, err := tx.Exec(ctx, `
UPDATE integration_inbox
SET processed_at = NOW()
WHERE consumer_name = $1 AND event_id = $2
`, c.group(), envelope.ID); err != nil {
			return err
		}
	}
	if err := tx.Commit(ctx); err != nil {
		return err
	}
	_ = c.clearFailure(ctx, envelope.ID)
	return c.Redis.XAck(ctx, c.stream(), c.group(), message.ID).Err()
}

func (c *Consumer) fail(ctx context.Context, message redis.XMessage, eventID string, processErr error) error {
	if c.DB == nil {
		return processErr
	}
	if eventID == "" {
		eventID = "redis:" + message.ID
	}
	var attempts int
	err := c.DB.QueryRow(ctx, `
INSERT INTO integration_dead_letters(
    consumer_name, event_id, stream_entry_id, payload, attempts, last_error, first_failed_at, last_failed_at
)
VALUES($1, $2, $3, $4::jsonb, 1, $5, NOW(), NOW())
ON CONFLICT(consumer_name, event_id)
DO UPDATE SET
    attempts = integration_dead_letters.attempts + 1,
    last_error = EXCLUDED.last_error,
    last_failed_at = NOW()
RETURNING attempts
`, c.group(), eventID, message.ID, validJSONPayload(message.Values["event"]), truncateError(processErr)).Scan(&attempts)
	if err != nil {
		return processErr
	}
	maxAttempts := c.MaxAttempts
	if maxAttempts <= 0 {
		maxAttempts = 5
	}
	if attempts >= maxAttempts {
		_ = c.Redis.XAck(ctx, c.stream(), c.group(), message.ID).Err()
	}
	return processErr
}

func (c *Consumer) clearFailure(ctx context.Context, eventID string) error {
	_, err := c.DB.Exec(ctx, `DELETE FROM integration_dead_letters WHERE consumer_name = $1 AND event_id = $2`, c.group(), eventID)
	return err
}

func (c *Consumer) stream() string {
	if c.Stream != "" {
		return c.Stream
	}
	return "ojos:integration:problem:v1"
}

func (c *Consumer) group() string {
	if c.Group != "" {
		return c.Group
	}
	return "judge-api.problem-projection.v1"
}

func (c *Consumer) consumerName() string {
	if c.ConsumerName != "" {
		return c.ConsumerName
	}
	return "consumer-" + newEventID()
}

func validJSONPayload(value any) string {
	raw := fmt.Sprint(value)
	if json.Valid([]byte(raw)) {
		return raw
	}
	encoded, _ := json.Marshal(map[string]string{"raw": raw})
	return string(encoded)
}
