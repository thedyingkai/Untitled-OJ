package eventing

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
)

const (
	CloudEventsSpecVersion = "1.0"
	JSONContentType        = "application/json"
)

// Envelope is the language-neutral integration-event boundary shared by all
// services. AggregateVersion is an OJOS CloudEvents extension used to make
// full-snapshot projections deterministic under duplicate and out-of-order
// delivery.
type Envelope struct {
	SpecVersion      string          `json:"specversion"`
	ID               string          `json:"id"`
	Source           string          `json:"source"`
	Type             string          `json:"type"`
	Subject          string          `json:"subject"`
	Time             time.Time       `json:"time"`
	DataContentType  string          `json:"datacontenttype"`
	DataSchema       string          `json:"dataschema,omitempty"`
	AggregateVersion int64           `json:"aggregateversion"`
	TraceParent      string          `json:"traceparent,omitempty"`
	TraceState       string          `json:"tracestate,omitempty"`
	Data             json.RawMessage `json:"data"`
}

func newEnvelope(ctx context.Context, source, eventType, subject, dataSchema string, aggregateVersion int64, data any) (Envelope, error) {
	payload, err := json.Marshal(data)
	if err != nil {
		return Envelope{}, fmt.Errorf("marshal integration event data: %w", err)
	}

	carrier := propagation.MapCarrier{}
	otel.GetTextMapPropagator().Inject(ctx, carrier)
	eventType = strings.TrimSpace(eventType)
	dataSchema = strings.TrimSpace(dataSchema)
	envelope := Envelope{
		SpecVersion:      CloudEventsSpecVersion,
		ID:               newEventID(),
		Source:           strings.TrimSpace(source),
		Type:             eventType,
		Subject:          strings.TrimSpace(subject),
		Time:             time.Now().UTC(),
		DataContentType:  JSONContentType,
		DataSchema:       dataSchema,
		AggregateVersion: aggregateVersion,
		TraceParent:      strings.TrimSpace(carrier.Get("traceparent")),
		TraceState:       strings.TrimSpace(carrier.Get("tracestate")),
		Data:             payload,
	}
	if err := envelope.Validate(); err != nil {
		return Envelope{}, err
	}
	return envelope, nil
}

func (e Envelope) Validate() error {
	return e.validateBase()
}

func (e Envelope) validateBase() error {
	if e.SpecVersion != CloudEventsSpecVersion {
		return fmt.Errorf("unsupported CloudEvents specversion %q", e.SpecVersion)
	}
	if strings.TrimSpace(e.ID) == "" || strings.TrimSpace(e.Source) == "" || strings.TrimSpace(e.Type) == "" || strings.TrimSpace(e.Subject) == "" {
		return errors.New("integration event identity is incomplete")
	}
	if e.AggregateVersion <= 0 {
		return errors.New("integration event aggregateversion must be positive")
	}
	if e.Time.IsZero() {
		return errors.New("integration event time is required")
	}
	if e.DataContentType != JSONContentType {
		return fmt.Errorf("unsupported integration event content type %q", e.DataContentType)
	}
	if len(e.Data) == 0 || !json.Valid(e.Data) {
		return errors.New("integration event data must be valid JSON")
	}
	return nil
}

func newEventID() string {
	var raw [16]byte
	if _, err := rand.Read(raw[:]); err == nil {
		return hex.EncodeToString(raw[:])
	}
	return fmt.Sprintf("event-%d", time.Now().UTC().UnixNano())
}
