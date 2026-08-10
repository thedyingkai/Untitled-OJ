package logic

import (
	"errors"
	"net/http"
	"strings"
	"testing"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	sharedmw "ojos-shared/middleware"
)

func TestSubmissionProjectionGateRequiresCompleteManagedArtifactByDefault(t *testing.T) {
	problem := &repository.ProblemMeta{
		ID:                       17,
		PackageDir:               "/legacy/problem-17",
		AggregateVersion:         2,
		PackageRevision:          1,
		PackageArtifactURI:       "storage://problems/package.zip",
		PackageArtifactSHA256:    strings.Repeat("a", 64),
		PackageArtifactSizeBytes: 123,
	}
	if err := ensureSubmissionProblemProjection(&svc.ServiceContext{}, problem); err != nil {
		t.Fatalf("complete managed projection was rejected: %v", err)
	}

	problem.PackageArtifactSizeBytes = 0
	err := ensureSubmissionProblemProjection(&svc.ServiceContext{}, problem)
	if err == nil || !strings.Contains(err.Error(), "incomplete or invalid") {
		t.Fatalf("partial projection did not fail with a reconcile error: %v", err)
	}
	var coded sharedmw.CodedHTTPError
	if !errors.As(err, &coded) || coded.HTTPStatus() != http.StatusConflict || coded.ErrorCode() != 40921 {
		t.Fatalf("projection gate did not expose stable HTTP conflict details: %#v", err)
	}
}

func TestSubmissionProjectionGateAllowsPristineLegacyOnlyWithExplicitDevelopmentOptIn(t *testing.T) {
	legacy := &repository.ProblemMeta{ID: 18, PackageDir: "/legacy/problem-18"}
	if err := ensureSubmissionProblemProjection(&svc.ServiceContext{}, legacy); err == nil || !strings.Contains(err.Error(), "backfill/reconcile") {
		t.Fatalf("default legacy submission was not fail-closed: %v", err)
	}

	compatibility := &svc.ServiceContext{Config: config.Config{
		ProblemProjection: config.ProblemProjectionConfig{AllowLegacyPackageDir: true},
	}}
	if err := ensureSubmissionProblemProjection(compatibility, legacy); err != nil {
		t.Fatalf("explicit development compatibility was rejected: %v", err)
	}

	legacy.PackageArtifactSHA256 = strings.Repeat("a", 64)
	if err := ensureSubmissionProblemProjection(compatibility, legacy); err == nil || !strings.Contains(err.Error(), "incomplete or invalid") {
		t.Fatalf("compatibility mode masked partial projection corruption: %v", err)
	}
}
