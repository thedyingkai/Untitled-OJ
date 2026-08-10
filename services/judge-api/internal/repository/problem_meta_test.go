package repository

import (
	"strings"
	"testing"
)

func TestProblemMetaManagedArtifactIdentity(t *testing.T) {
	problem := ProblemMeta{
		AggregateVersion:         1,
		PackageRevision:          1,
		PackageArtifactURI:       "storage://problems/package.zip",
		PackageArtifactSHA256:    strings.Repeat("a", 64),
		PackageArtifactSizeBytes: 1,
	}
	if !problem.HasManagedPackageArtifact() {
		t.Fatal("complete managed artifact was not recognized")
	}
	problem.PackageArtifactSHA256 = strings.Repeat("A", 64)
	if problem.HasManagedPackageArtifact() {
		t.Fatal("uppercase/non-canonical artifact digest was accepted")
	}
	if !problem.HasAnyProjectionArtifactState() {
		t.Fatal("invalid partial projection state was not detected")
	}
}
