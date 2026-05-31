package events

import (
	"context"
	"encoding/json"

	"github.com/nats-io/nats.go"
	"go.opentelemetry.io/otel"
)

type Bus struct {
	conn     *nats.Conn
	producer string
}

func NewBusByURL(url string, producer string) (*Bus, error) {
	nc, err := nats.Connect(url)
	if err != nil {
		return nil, err
	}

	return &Bus{
		conn:     nc,
		producer: producer,
	}, nil
}

func (b *Bus) Publish(ctx context.Context, subject string, eventType string, payload any) error {
	tracer := otel.Tracer("events")
	_, span := tracer.Start(ctx, "nats.publish")
	defer span.End()

	event, err := New(eventType, b.producer, payload)
	if err != nil {
		return err
	}

	data, err := json.Marshal(event)
	if err != nil {
		return err
	}

	return b.conn.Publish(subject, data)
}

func (b *Bus) Close() {
	if b != nil && b.conn != nil {
		b.conn.Close()
	}
}
