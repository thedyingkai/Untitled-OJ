package eventing

import (
	"context"
	"encoding/json"
	"os"
	"strings"
	"testing"
	"time"
)

func validProblemSnapshot() ProblemSnapshotData {
	return ProblemSnapshotData{
		ProblemID:        42,
		AggregateVersion: 3,
		PackageRevision:  2,
		ProblemNo:        "P42",
		Title:            "strict event",
		ProblemType:      "traditional",
		Status:           "published",
		Visibility:       "public",
		CreatedBy:        7,
		TimeLimitMS:      1000,
		MemoryLimitMB:    256,
		ManifestSHA256:   strings.Repeat("b", 64),
		PackageArtifact: ArtifactRef{
			URI:         "storage://problems/package-sha256-" + strings.Repeat("a", 64) + ".zip",
			SHA256:      strings.Repeat("a", 64),
			SizeBytes:   10,
			ContentType: "application/zip",
		},
		SourceUpdatedAtUTC: time.Now().UTC(),
	}
}

func TestProblemSnapshotEnvelopeRoundTripUsesBoundSchema(t *testing.T) {
	data := validProblemSnapshot()
	envelope, err := NewEnvelope(context.Background(), "ojos://problem-service", ProblemSnapshotV1, "problem/42", "", 3, data)
	if err != nil {
		t.Fatal(err)
	}
	if envelope.DataSchema != ProblemSnapshotSchemaV1 {
		t.Fatalf("known event did not bind checked-in schema: %#v", envelope)
	}
	raw, err := json.Marshal(envelope)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := DecodeEnvelopeStrict(raw)
	if err != nil {
		t.Fatal(err)
	}
	snapshot, err := DecodeProblemSnapshotData(decoded)
	if err != nil {
		t.Fatal(err)
	}
	if snapshot.AggregateVersion != 3 || snapshot.PackageArtifact.SHA256 != data.PackageArtifact.SHA256 {
		t.Fatalf("unexpected decoded snapshot: %#v", snapshot)
	}
}

func TestProblemEventsRejectUnknownFieldsAndInvalidArtifactIdentity(t *testing.T) {
	data := validProblemSnapshot()
	envelope, err := NewEnvelope(context.Background(), "ojos://problem-service", ProblemSnapshotV1, "problem/42", "", 3, data)
	if err != nil {
		t.Fatal(err)
	}

	var rawData map[string]any
	if err := json.Unmarshal(envelope.Data, &rawData); err != nil {
		t.Fatal(err)
	}
	rawData["future_field"] = true
	envelope.Data, _ = json.Marshal(rawData)
	if err := envelope.Validate(); err == nil || !strings.Contains(err.Error(), "unknown field") {
		t.Fatalf("unknown snapshot field was not rejected: %v", err)
	}
	delete(rawData, "future_field")
	delete(rawData, "created_by")
	envelope.Data, _ = json.Marshal(rawData)
	if err := envelope.Validate(); err == nil || !strings.Contains(err.Error(), "missing required") {
		t.Fatalf("missing required created_by was not rejected: %v", err)
	}

	data = validProblemSnapshot()
	data.PackageArtifact.SHA256 = strings.Repeat("A", 64)
	if _, err := NewEnvelope(context.Background(), "ojos://problem-service", ProblemSnapshotV1, "problem/42", "", 3, data); err == nil || !strings.Contains(err.Error(), "lowercase sha256") {
		t.Fatalf("uppercase artifact sha256 was not rejected: %v", err)
	}
	data = validProblemSnapshot()
	data.PackageArtifact.SizeBytes = 0
	if _, err := NewEnvelope(context.Background(), "ojos://problem-service", ProblemSnapshotV1, "problem/42", "", 3, data); err == nil || !strings.Contains(err.Error(), "positive size_bytes") {
		t.Fatalf("zero artifact size was not rejected: %v", err)
	}
}

func TestProblemEventsRejectUnknownEnvelopeAndWrongSchema(t *testing.T) {
	data := validProblemSnapshot()
	envelope, err := NewEnvelope(context.Background(), "ojos://problem-service", ProblemSnapshotV1, "problem/42", "", 3, data)
	if err != nil {
		t.Fatal(err)
	}
	envelope.DataSchema = ProblemDeletedSchemaV1
	if err := envelope.Validate(); err == nil || !strings.Contains(err.Error(), "requires dataschema") {
		t.Fatalf("wrong type/schema binding was not rejected: %v", err)
	}

	validEnvelope, err := NewEnvelope(context.Background(), "ojos://problem-service", ProblemSnapshotV1, "problem/42", "", 3, data)
	if err != nil {
		t.Fatal(err)
	}
	raw, _ := json.Marshal(validEnvelope)
	var object map[string]any
	if err := json.Unmarshal(raw, &object); err != nil {
		t.Fatal(err)
	}
	object["caller_is_admin"] = true
	raw, _ = json.Marshal(object)
	if _, err := DecodeEnvelopeStrict(raw); err == nil || !strings.Contains(err.Error(), "unknown field") {
		t.Fatalf("unknown envelope field was not rejected: %v", err)
	}
}

func TestProblemDeletedEventRequiresExactPayloadAndIdentity(t *testing.T) {
	deleted := ProblemDeletedData{ProblemID: 42, AggregateVersion: 4}
	envelope, err := NewEnvelope(context.Background(), "ojos://problem-service", ProblemDeletedV1, "problem/42", "", 4, deleted)
	if err != nil {
		t.Fatal(err)
	}
	if envelope.DataSchema != ProblemDeletedSchemaV1 {
		t.Fatalf("deleted event did not bind checked-in schema: %#v", envelope)
	}
	if _, err := DecodeProblemDeletedData(envelope); err != nil {
		t.Fatal(err)
	}
	envelope.Subject = "problem/43"
	if err := envelope.Validate(); err == nil || !strings.Contains(err.Error(), "subject") {
		t.Fatalf("mismatched deleted subject was not rejected: %v", err)
	}
}

func TestProblemEventSchemaIdentifiersMatchCheckedInSchemas(t *testing.T) {
	tests := []struct {
		path string
		want string
	}{
		{"../../../schemas/events/problem-snapshot-v1.schema.json", ProblemSnapshotSchemaV1},
		{"../../../schemas/events/problem-deleted-v1.schema.json", ProblemDeletedSchemaV1},
	}
	for _, test := range tests {
		raw, err := os.ReadFile(test.path)
		if err != nil {
			t.Fatalf("read checked-in schema %s: %v", test.path, err)
		}
		var schema struct {
			ID                   string `json:"$id"`
			AdditionalProperties bool   `json:"additionalProperties"`
		}
		if err := json.Unmarshal(raw, &schema); err != nil {
			t.Fatalf("decode checked-in schema %s: %v", test.path, err)
		}
		if schema.ID != test.want || schema.AdditionalProperties {
			t.Fatalf("checked-in schema drift for %s: %#v", test.path, schema)
		}
	}
}
