package repository

import (
	"strings"
	"testing"

	"ojos-problem-events/problemv1"
)

func TestNormalizedArtifactIntentAcceptsPackageAndContentKeys(t *testing.T) {
	digest := strings.Repeat("a", 64)
	for _, artifact := range []problemv1.ArtifactRef{
		{
			URI:    "storage://problems/package-sha256-" + digest + ".zip",
			SHA256: digest, SizeBytes: 1,
		},
		{
			URI:    "storage://problems/problem-17-objects-sha256-" + digest,
			SHA256: digest, SizeBytes: 0,
		},
	} {
		if _, _, err := normalizedArtifactIntent(artifact); err != nil {
			t.Fatalf("valid artifact intent rejected: artifact=%#v err=%v", artifact, err)
		}
	}
}

func TestNormalizedArtifactIntentRejectsAmbiguousKeysAndInvalidSizes(t *testing.T) {
	digest := strings.Repeat("b", 64)
	for _, artifact := range []problemv1.ArtifactRef{
		{URI: "storage://problems/package-sha256-" + digest + ".zip", SHA256: digest, SizeBytes: 0},
		{URI: "storage://Problems/problem-1-objects-sha256-" + digest, SHA256: digest, SizeBytes: 1},
		{URI: "storage://p/problem-1-objects-sha256-" + digest, SHA256: digest, SizeBytes: 1},
		{URI: "storage://problem_bucket/problem-1-objects-sha256-" + digest, SHA256: digest, SizeBytes: 1},
		{URI: "storage://problems/problem-0-objects-sha256-" + digest, SHA256: digest, SizeBytes: 1},
		{URI: "storage://problems/problem-01-objects-sha256-" + digest, SHA256: digest, SizeBytes: 1},
		{URI: "storage://problems/problem-1-objects-sha256-" + digest, SHA256: digest, SizeBytes: -1},
		{URI: "storage://problems/arbitrary-" + digest, SHA256: digest, SizeBytes: 1},
	} {
		if _, _, err := normalizedArtifactIntent(artifact); err == nil {
			t.Fatalf("invalid artifact intent accepted: %#v", artifact)
		}
	}
}
