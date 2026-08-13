package eventing

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

type fixturePayload struct {
	ID   int64  `json:"id"`
	Name string `json:"name"`
}

func fixtureCodec(t *testing.T) Codec[fixturePayload] {
	t.Helper()
	codec, err := NewCodec(
		EventDescriptor{Type: "io.example.fixture.v1", DataSchema: "https://schemas.example.test/fixture-v1.json"},
		nil,
		func(envelope Envelope, value fixturePayload) error {
			if value.ID <= 0 || envelope.Subject != "fixture/1" {
				return &fixtureValidationError{}
			}
			return nil
		},
	)
	if err != nil {
		t.Fatal(err)
	}
	return codec
}

type fixtureValidationError struct{}

func (*fixtureValidationError) Error() string { return "invalid fixture payload identity" }

func TestCodecBindsTypeSchemaAndRoundTripsStrictly(t *testing.T) {
	codec := fixtureCodec(t)
	envelope, err := codec.NewEnvelope(context.Background(), "ojos://fixture", "fixture/1", 1, fixturePayload{ID: 1, Name: "test"})
	if err != nil {
		t.Fatal(err)
	}
	if envelope.Type != codec.Descriptor().Type || envelope.DataSchema != codec.Descriptor().DataSchema {
		t.Fatalf("codec did not bind descriptor: %#v", envelope)
	}
	raw, err := json.Marshal(envelope)
	if err != nil {
		t.Fatal(err)
	}
	decodedEnvelope, decoded, err := codec.DecodeEnvelope(raw)
	if err != nil {
		t.Fatal(err)
	}
	if decodedEnvelope.ID != envelope.ID || decoded.ID != 1 || decoded.Name != "test" {
		t.Fatalf("unexpected round trip: %#v %#v", decodedEnvelope, decoded)
	}
}

func TestCodecProducesOpaqueTypedEvent(t *testing.T) {
	codec := fixtureCodec(t)
	event, err := codec.NewEvent(context.Background(), "ojos://fixture", "fixture/1", 1, fixturePayload{ID: 1})
	if err != nil {
		t.Fatal(err)
	}
	envelope := event.Envelope()
	if envelope.Type != codec.Descriptor().Type || envelope.DataSchema != codec.Descriptor().DataSchema {
		t.Fatalf("typed event lost codec binding: %#v", envelope)
	}
}

func TestCodecRejectsWrongTypeSchemaAndUnknownPayloadFields(t *testing.T) {
	codec := fixtureCodec(t)
	envelope, err := codec.NewEnvelope(context.Background(), "ojos://fixture", "fixture/1", 1, fixturePayload{ID: 1})
	if err != nil {
		t.Fatal(err)
	}
	envelope.DataSchema = "https://schemas.example.test/other.json"
	if _, err := codec.Decode(envelope); err == nil || !strings.Contains(err.Error(), "requires dataschema") {
		t.Fatalf("wrong schema accepted: %v", err)
	}
	envelope.DataSchema = codec.Descriptor().DataSchema
	envelope.Type = "io.example.other.v1"
	if _, err := codec.Decode(envelope); err == nil || !strings.Contains(err.Error(), "expected event type") {
		t.Fatalf("wrong type accepted: %v", err)
	}
	envelope.Type = codec.Descriptor().Type
	envelope.Data = json.RawMessage(`{"id":1,"name":"test","future":true}`)
	if _, err := codec.Decode(envelope); err == nil || !strings.Contains(err.Error(), "unknown field") {
		t.Fatalf("unknown payload field accepted: %v", err)
	}
}

func TestDecodeEnvelopeStrictRejectsUnknownEnvelopeFields(t *testing.T) {
	codec := fixtureCodec(t)
	envelope, err := codec.NewEnvelope(context.Background(), "ojos://fixture", "fixture/1", 1, fixturePayload{ID: 1})
	if err != nil {
		t.Fatal(err)
	}
	raw, _ := json.Marshal(envelope)
	var object map[string]any
	_ = json.Unmarshal(raw, &object)
	object["caller_is_admin"] = true
	raw, _ = json.Marshal(object)
	if _, err := DecodeEnvelopeStrict(raw); err == nil || !strings.Contains(err.Error(), "unknown field") {
		t.Fatalf("unknown envelope field accepted: %v", err)
	}
}

func TestCodecDescriptorMustBindTypeAndSchema(t *testing.T) {
	for _, descriptor := range []EventDescriptor{
		{DataSchema: "https://schemas.example.test/fixture.json"},
		{Type: "io.example.fixture.v1"},
	} {
		if _, err := NewCodec[fixturePayload](descriptor, nil, nil); err == nil {
			t.Fatalf("incomplete descriptor accepted: %#v", descriptor)
		}
	}
}
