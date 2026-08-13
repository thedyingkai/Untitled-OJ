package eventing

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
)

// EventDescriptor is the immutable wire identity of one event major. Domain
// packages own descriptors; the shared eventing runtime does not maintain an
// event-type registry or inspect domain payloads.
type EventDescriptor struct {
	Type       string
	DataSchema string
}

func (d EventDescriptor) Validate() error {
	if strings.TrimSpace(d.Type) == "" {
		return errors.New("event descriptor type is required")
	}
	if strings.TrimSpace(d.DataSchema) == "" {
		return errors.New("event descriptor dataschema is required")
	}
	return nil
}

type PayloadDecoder[T any] func(json.RawMessage) (T, error)
type PayloadValidator[T any] func(Envelope, T) error

// Codec binds a Go payload type to exactly one event type and dataschema. A
// codec may provide a generated decoder so required-field and JSON Schema
// constraints remain distinguishable from Go zero values.
type Codec[T any] struct {
	descriptor EventDescriptor
	decode     PayloadDecoder[T]
	validate   PayloadValidator[T]
}

func NewCodec[T any](descriptor EventDescriptor, decoder PayloadDecoder[T], validator PayloadValidator[T]) (Codec[T], error) {
	descriptor.Type = strings.TrimSpace(descriptor.Type)
	descriptor.DataSchema = strings.TrimSpace(descriptor.DataSchema)
	if err := descriptor.Validate(); err != nil {
		return Codec[T]{}, err
	}
	if decoder == nil {
		decoder = DecodeJSONStrict[T]
	}
	return Codec[T]{descriptor: descriptor, decode: decoder, validate: validator}, nil
}

func MustCodec[T any](descriptor EventDescriptor, decoder PayloadDecoder[T], validator PayloadValidator[T]) Codec[T] {
	codec, err := NewCodec(descriptor, decoder, validator)
	if err != nil {
		panic(err)
	}
	return codec
}

func (c Codec[T]) Descriptor() EventDescriptor {
	return c.descriptor
}

func (c Codec[T]) NewEnvelope(ctx context.Context, source, subject string, aggregateVersion int64, data T) (Envelope, error) {
	envelope, err := newEnvelope(
		ctx,
		source,
		c.descriptor.Type,
		subject,
		c.descriptor.DataSchema,
		aggregateVersion,
		data,
	)
	if err != nil {
		return Envelope{}, err
	}
	if _, err := c.Decode(envelope); err != nil {
		return Envelope{}, err
	}
	return envelope, nil
}

func (c Codec[T]) NewEvent(ctx context.Context, source, subject string, aggregateVersion int64, data T) (TypedEvent, error) {
	envelope, err := c.NewEnvelope(ctx, source, subject, aggregateVersion, data)
	if err != nil {
		return TypedEvent{}, err
	}
	return TypedEvent{envelope: envelope}, nil
}

func (c Codec[T]) Bind(envelope Envelope) (TypedEvent, error) {
	if _, err := c.Decode(envelope); err != nil {
		return TypedEvent{}, err
	}
	return TypedEvent{envelope: envelope}, nil
}

func (c Codec[T]) Decode(envelope Envelope) (T, error) {
	var zero T
	if err := c.descriptor.Validate(); err != nil {
		return zero, err
	}
	if err := envelope.Validate(); err != nil {
		return zero, err
	}
	if envelope.Type != c.descriptor.Type {
		return zero, fmt.Errorf("expected event type %q, got %q", c.descriptor.Type, envelope.Type)
	}
	if envelope.DataSchema != c.descriptor.DataSchema {
		return zero, fmt.Errorf("event type %q requires dataschema %q", c.descriptor.Type, c.descriptor.DataSchema)
	}
	value, err := c.decode(envelope.Data)
	if err != nil {
		return zero, err
	}
	if c.validate != nil {
		if err := c.validate(envelope, value); err != nil {
			return zero, err
		}
	}
	return value, nil
}

func (c Codec[T]) DecodeEnvelope(payload []byte) (Envelope, T, error) {
	var zero T
	envelope, err := DecodeEnvelopeStrict(payload)
	if err != nil {
		return envelope, zero, err
	}
	value, err := c.Decode(envelope)
	return envelope, value, err
}

// DecodeEnvelopeStrict is the broker/HTTP boundary. It rejects unknown
// envelope fields and validates only the generic envelope. The selected domain
// codec performs the type/schema and payload validation.
func DecodeEnvelopeStrict(payload []byte) (Envelope, error) {
	var envelope Envelope
	if err := decodeStrictJSON(payload, &envelope); err != nil {
		return envelope, fmt.Errorf("decode integration event envelope: %w", err)
	}
	if err := envelope.Validate(); err != nil {
		return envelope, err
	}
	return envelope, nil
}

func DecodeJSONStrict[T any](payload json.RawMessage) (T, error) {
	var value T
	if err := decodeStrictJSON(payload, &value); err != nil {
		return value, err
	}
	return value, nil
}

func decodeStrictJSON(payload []byte, target any) error {
	decoder := json.NewDecoder(bytes.NewReader(payload))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(target); err != nil {
		return err
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		if err == nil {
			return errors.New("multiple JSON values are not allowed")
		}
		return err
	}
	return nil
}
