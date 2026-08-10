package eventing

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strconv"
	"strings"
	"time"
)

// DecodeEnvelopeStrict is the integration boundary for broker and HTTP event
// payloads. It rejects unknown envelope fields before applying the event-type
// specific schema checks performed by Envelope.Validate.
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

// DecodeProblemSnapshotData validates and decodes a snapshot using the same
// constraints as problem-snapshot-v1.schema.json. Pointers on the private wire
// type distinguish an omitted required property from a valid zero created_by.
func DecodeProblemSnapshotData(envelope Envelope) (ProblemSnapshotData, error) {
	if err := envelope.validateBase(); err != nil {
		return ProblemSnapshotData{}, err
	}
	if envelope.Type != ProblemSnapshotV1 {
		return ProblemSnapshotData{}, fmt.Errorf("expected event type %q, got %q", ProblemSnapshotV1, envelope.Type)
	}
	if envelope.DataSchema != ProblemSnapshotSchemaV1 {
		return ProblemSnapshotData{}, fmt.Errorf("event type %q requires dataschema %q", ProblemSnapshotV1, ProblemSnapshotSchemaV1)
	}
	return decodeProblemSnapshotData(envelope)
}

// DecodeProblemDeletedData validates and decodes a tombstone using the same
// constraints as problem-deleted-v1.schema.json.
func DecodeProblemDeletedData(envelope Envelope) (ProblemDeletedData, error) {
	if err := envelope.validateBase(); err != nil {
		return ProblemDeletedData{}, err
	}
	if envelope.Type != ProblemDeletedV1 {
		return ProblemDeletedData{}, fmt.Errorf("expected event type %q, got %q", ProblemDeletedV1, envelope.Type)
	}
	if envelope.DataSchema != ProblemDeletedSchemaV1 {
		return ProblemDeletedData{}, fmt.Errorf("event type %q requires dataschema %q", ProblemDeletedV1, ProblemDeletedSchemaV1)
	}
	return decodeProblemDeletedData(envelope)
}

func validateKnownProblemEvent(envelope Envelope) error {
	expectedSchema, known := ExpectedDataSchema(envelope.Type)
	if !known {
		return nil
	}
	if envelope.DataSchema != expectedSchema {
		return fmt.Errorf("event type %q requires dataschema %q", envelope.Type, expectedSchema)
	}
	switch envelope.Type {
	case ProblemSnapshotV1:
		_, err := decodeProblemSnapshotData(envelope)
		return err
	case ProblemDeletedV1:
		_, err := decodeProblemDeletedData(envelope)
		return err
	default:
		return nil
	}
}

type problemSnapshotWire struct {
	ProblemID          *int64           `json:"problem_id"`
	AggregateVersion   *int64           `json:"aggregate_version"`
	PackageRevision    *int64           `json:"package_revision"`
	ProblemNo          string           `json:"problem_no"`
	Title              string           `json:"title"`
	ProblemType        string           `json:"problem_type"`
	Status             *string          `json:"status"`
	Visibility         *string          `json:"visibility"`
	CreatedBy          *int64           `json:"created_by"`
	TimeLimitMS        *int             `json:"time_limit_ms"`
	MemoryLimitMB      *int             `json:"memory_limit_mb"`
	ManifestSHA256     string           `json:"manifest_sha256"`
	PackageArtifact    *artifactRefWire `json:"package_artifact"`
	SourceUpdatedAtUTC *time.Time       `json:"source_updated_at"`
}

type artifactRefWire struct {
	URI         *string `json:"uri"`
	SHA256      *string `json:"sha256"`
	SizeBytes   *int64  `json:"size_bytes"`
	ContentType *string `json:"content_type"`
}

func decodeProblemSnapshotData(envelope Envelope) (ProblemSnapshotData, error) {
	var wire problemSnapshotWire
	if err := decodeStrictJSON(envelope.Data, &wire); err != nil {
		return ProblemSnapshotData{}, fmt.Errorf("decode problem snapshot v1: %w", err)
	}
	if wire.ProblemID == nil || *wire.ProblemID <= 0 ||
		wire.AggregateVersion == nil || *wire.AggregateVersion <= 0 ||
		wire.PackageRevision == nil || *wire.PackageRevision <= 0 ||
		wire.Status == nil || !oneOf(*wire.Status, "draft", "ready", "published", "archived") ||
		wire.Visibility == nil || !oneOf(*wire.Visibility, "private", "public") ||
		wire.CreatedBy == nil || *wire.CreatedBy < 0 || wire.PackageArtifact == nil ||
		wire.SourceUpdatedAtUTC == nil || wire.SourceUpdatedAtUTC.IsZero() {
		return ProblemSnapshotData{}, errors.New("problem snapshot v1 is missing required or valid properties")
	}
	if wire.TimeLimitMS != nil && *wire.TimeLimitMS <= 0 {
		return ProblemSnapshotData{}, errors.New("problem snapshot time_limit_ms must be positive when present")
	}
	if wire.MemoryLimitMB != nil && *wire.MemoryLimitMB <= 0 {
		return ProblemSnapshotData{}, errors.New("problem snapshot memory_limit_mb must be positive when present")
	}
	artifact, err := validateArtifactRefWire(*wire.PackageArtifact)
	if err != nil {
		return ProblemSnapshotData{}, fmt.Errorf("problem snapshot package_artifact: %w", err)
	}
	if *wire.AggregateVersion != envelope.AggregateVersion {
		return ProblemSnapshotData{}, errors.New("problem snapshot aggregate_version does not match CloudEvents aggregateversion")
	}
	if envelope.Subject != "problem/"+strconv.FormatInt(*wire.ProblemID, 10) {
		return ProblemSnapshotData{}, errors.New("problem snapshot subject does not match problem_id")
	}

	data := ProblemSnapshotData{
		ProblemID:          *wire.ProblemID,
		AggregateVersion:   *wire.AggregateVersion,
		PackageRevision:    *wire.PackageRevision,
		ProblemNo:          wire.ProblemNo,
		Title:              wire.Title,
		ProblemType:        wire.ProblemType,
		Status:             *wire.Status,
		Visibility:         *wire.Visibility,
		CreatedBy:          *wire.CreatedBy,
		ManifestSHA256:     wire.ManifestSHA256,
		PackageArtifact:    artifact,
		SourceUpdatedAtUTC: wire.SourceUpdatedAtUTC.UTC(),
	}
	if wire.TimeLimitMS != nil {
		data.TimeLimitMS = *wire.TimeLimitMS
	}
	if wire.MemoryLimitMB != nil {
		data.MemoryLimitMB = *wire.MemoryLimitMB
	}
	return data, nil
}

type problemDeletedWire struct {
	ProblemID        *int64 `json:"problem_id"`
	AggregateVersion *int64 `json:"aggregate_version"`
}

func decodeProblemDeletedData(envelope Envelope) (ProblemDeletedData, error) {
	var wire problemDeletedWire
	if err := decodeStrictJSON(envelope.Data, &wire); err != nil {
		return ProblemDeletedData{}, fmt.Errorf("decode problem deleted v1: %w", err)
	}
	if wire.ProblemID == nil || *wire.ProblemID <= 0 || wire.AggregateVersion == nil || *wire.AggregateVersion <= 0 {
		return ProblemDeletedData{}, errors.New("problem deleted v1 is missing required or valid properties")
	}
	if *wire.AggregateVersion != envelope.AggregateVersion {
		return ProblemDeletedData{}, errors.New("problem deleted aggregate_version does not match CloudEvents aggregateversion")
	}
	if envelope.Subject != "problem/"+strconv.FormatInt(*wire.ProblemID, 10) {
		return ProblemDeletedData{}, errors.New("problem deleted subject does not match problem_id")
	}
	return ProblemDeletedData{ProblemID: *wire.ProblemID, AggregateVersion: *wire.AggregateVersion}, nil
}

func validateArtifactRefWire(wire artifactRefWire) (ArtifactRef, error) {
	if wire.URI == nil || strings.TrimSpace(*wire.URI) == "" || wire.SHA256 == nil ||
		!isLowerHexSHA256(*wire.SHA256) || wire.SizeBytes == nil || *wire.SizeBytes <= 0 ||
		wire.ContentType == nil || *wire.ContentType != "application/zip" {
		return ArtifactRef{}, errors.New("uri, lowercase sha256, positive size_bytes, and application/zip content_type are required")
	}
	return ArtifactRef{
		URI:         *wire.URI,
		SHA256:      *wire.SHA256,
		SizeBytes:   *wire.SizeBytes,
		ContentType: *wire.ContentType,
	}, nil
}

func isLowerHexSHA256(value string) bool {
	if len(value) != 64 {
		return false
	}
	for _, char := range value {
		if (char < '0' || char > '9') && (char < 'a' || char > 'f') {
			return false
		}
	}
	return true
}

func oneOf(value string, allowed ...string) bool {
	for _, candidate := range allowed {
		if value == candidate {
			return true
		}
	}
	return false
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
