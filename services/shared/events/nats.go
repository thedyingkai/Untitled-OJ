package events

import (
	"context"
	"encoding/json"
	"fmt"

	"ojos-shared/config"

	"github.com/nats-io/nats.go"
	"go.opentelemetry.io/otel"
)

type Bus struct {
	conn     *nats.Conn
	producer string
}

func NewBus(cfg *config.Config) (*Bus, error) {
	if cfg.Nats.URL == "" {
		return nil, fmt.Errorf("nats url is empty")
	}

	conn, err := nats.Connect(cfg.Nats.URL)
	if err != nil {
		return nil, err
	}

	return &Bus{
		conn:     conn,
		producer: cfg.Service.Name,
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
	if b.conn != nil {
		b.conn.Close()
	}
}
