package problemv1

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func validSnapshot() Snapshot {
	return Snapshot{
		ProblemID: 42, AggregateVersion: 3, PackageRevision: 2,
		ProblemNo: "P42", Title: "strict event", ProblemType: "traditional",
		Status: "published", Visibility: "public", CreatedBy: 7,
		TimeLimitMS: 1000, MemoryLimitMB: 256, ManifestSHA256: strings.Repeat("b", 64),
		PackageArtifact:    ArtifactRef{URI: "storage://problems/package.zip", SHA256: strings.Repeat("a", 64), SizeBytes: 10, ContentType: "application/zip"},
		SourceUpdatedAtUTC: time.Now().UTC(),
	}
}

func TestGeneratedCodecsBindTypeSchemaAndValidatePayload(t *testing.T) {
	envelope, err := SnapshotCodec.NewEnvelope(context.Background(), "ojos://problem-service", "problem/42", 3, validSnapshot())
	if err != nil {
		t.Fatal(err)
	}
	if envelope.Type != SnapshotType || envelope.DataSchema != SnapshotSchema {
		t.Fatalf("codec did not bind its descriptor: %#v", envelope)
	}
	if _, err := SnapshotCodec.Decode(envelope); err != nil {
		t.Fatal(err)
	}
	envelope.DataSchema = DeletedSchema
	if _, err := SnapshotCodec.Decode(envelope); err == nil || !strings.Contains(err.Error(), "requires dataschema") {
		t.Fatalf("wrong schema accepted: %v", err)
	}
}

func TestGeneratedSnapshotCodecRejectsSchemaInvalidPayload(t *testing.T) {
	data := validSnapshot()
	envelope, err := SnapshotCodec.NewEnvelope(context.Background(), "ojos://problem-service", "problem/42", 3, data)
	if err != nil {
		t.Fatal(err)
	}
	var raw map[string]any
	_ = json.Unmarshal(envelope.Data, &raw)
	raw["future_field"] = true
	envelope.Data, _ = json.Marshal(raw)
	if _, err := SnapshotCodec.Decode(envelope); err == nil || !strings.Contains(err.Error(), "unknown field") {
		t.Fatalf("unknown field accepted: %v", err)
	}
	delete(raw, "future_field")
	delete(raw, "created_by")
	envelope.Data, _ = json.Marshal(raw)
	if _, err := SnapshotCodec.Decode(envelope); err == nil || !strings.Contains(err.Error(), "missing required") {
		t.Fatalf("missing required field accepted: %v", err)
	}
	data = validSnapshot()
	data.PackageArtifact.SHA256 = strings.Repeat("A", 64)
	if _, err := SnapshotCodec.NewEnvelope(context.Background(), "ojos://problem-service", "problem/42", 3, data); err == nil || !strings.Contains(err.Error(), "lowercase sha256") {
		t.Fatalf("uppercase artifact digest accepted: %v", err)
	}
	data = validSnapshot()
	data.PackageArtifact.SizeBytes = 0
	if _, err := SnapshotCodec.NewEnvelope(context.Background(), "ojos://problem-service", "problem/42", 3, data); err == nil || !strings.Contains(err.Error(), "positive size_bytes") {
		t.Fatalf("zero artifact size accepted: %v", err)
	}
}

func TestGeneratedCodecsRejectMismatchedIdentity(t *testing.T) {
	if _, err := SnapshotCodec.NewEnvelope(context.Background(), "ojos://problem-service", "problem/43", 3, validSnapshot()); err == nil || !strings.Contains(err.Error(), "subject") {
		t.Fatalf("snapshot subject mismatch accepted: %v", err)
	}
	deleted := Deleted{ProblemID: 42, AggregateVersion: 4}
	if _, err := DeletedCodec.NewEnvelope(context.Background(), "ojos://problem-service", "problem/42", 5, deleted); err == nil || !strings.Contains(err.Error(), "aggregate_version") {
		t.Fatalf("deleted aggregate mismatch accepted: %v", err)
	}
}

func TestGeneratedSchemaIdentityAndDigestMatchAuthorSource(t *testing.T) {
	tests := []struct{ name, id, digest string }{
		{"problem-snapshot-v1.schema.json", SnapshotSchema, SnapshotSchemaSHA256},
		{"problem-deleted-v1.schema.json", DeletedSchema, DeletedSchemaSHA256},
	}
	for _, test := range tests {
		path := filepath.Join("..", "..", "..", "..", "events", test.name)
		raw, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		var schema struct {
			ID                   string `json:"$id"`
			AdditionalProperties bool   `json:"additionalProperties"`
		}
		if err := json.Unmarshal(raw, &schema); err != nil {
			t.Fatal(err)
		}
		digest := sha256.Sum256(raw)
		if schema.ID != test.id || schema.AdditionalProperties || hex.EncodeToString(digest[:]) != test.digest {
			t.Fatalf("generated contract drift for %s", test.name)
		}
	}
}

func TestCompilerEventDescriptorsMatchDomainEventContract(t *testing.T) {
	generated, err := os.ReadFile(filepath.Join("..", "..", "events.go"))
	if err != nil {
		t.Fatal(err)
	}
	text := string(generated)
	for _, expected := range []string{
		`IoOjosProblemSnapshotV1V1Type = "` + SnapshotType + `"`,
		`IoOjosProblemDeletedV1V1Type = "` + DeletedType + `"`,
	} {
		if !strings.Contains(text, expected) {
			t.Fatalf("compiler event descriptor drifted from domain codec: %s", expected)
		}
	}
	for _, schemaFile := range []string{"problem-snapshot-v1.schema.json", "problem-deleted-v1.schema.json"} {
		serviceSchema, err := os.ReadFile(filepath.Join("..", "..", "..", "..", "events", schemaFile))
		if err != nil {
			t.Fatal(err)
		}
		platformSchema, err := os.ReadFile(filepath.Join("..", "..", "..", "..", "..", "..", "platform", "schemas", "events", schemaFile))
		if err != nil {
			t.Fatal(err)
		}
		var serviceValue, platformValue any
		if err := json.Unmarshal(serviceSchema, &serviceValue); err != nil {
			t.Fatal(err)
		}
		if err := json.Unmarshal(platformSchema, &platformValue); err != nil {
			t.Fatal(err)
		}
		serviceJSON, _ := json.Marshal(serviceValue)
		platformJSON, _ := json.Marshal(platformValue)
		if string(serviceJSON) != string(platformJSON) {
			t.Fatalf("service-owned %s drifted from the published compatibility schema", schemaFile)
		}
	}
}
