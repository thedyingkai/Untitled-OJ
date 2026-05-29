package events

import (
	"encoding/json"
	"time"

	"github.com/google/uuid"
)

type Event struct {
	ID        string          `json:"id"`
	Type      string          `json:"type"`
	Producer  string          `json:"producer"`
	Timestamp time.Time       `json:"timestamp"`
	Payload   json.RawMessage `json:"payload"`
}

func New(eventType string, producer string, payload any) (*Event, error) {
	data, err := json.Marshal(payload)
	if err != nil {
		return nil, err
	}

	return &Event{
		ID:        uuid.NewString(),
		Type:      eventType,
		Producer:  producer,
		Timestamp: time.Now(),
		Payload:   data,
	}, nil
}
