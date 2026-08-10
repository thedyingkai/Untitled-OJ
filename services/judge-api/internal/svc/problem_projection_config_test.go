package svc

import (
	"strings"
	"testing"

	"ojos-judge-api/internal/config"
)

func TestProductionRejectsLegacyProblemPackageCompatibility(t *testing.T) {
	valid := config.Config{}
	if err := validateProblemProjectionMode(valid, "production"); err != nil {
		t.Fatalf("strict production projection mode was rejected: %v", err)
	}

	legacy := config.Config{ProblemProjection: config.ProblemProjectionConfig{AllowLegacyPackageDir: true}}
	if err := validateProblemProjectionMode(legacy, "production"); err == nil || !strings.Contains(err.Error(), "only when OJOS_ENVIRONMENT=development") {
		t.Fatalf("production legacy projection mode was not rejected: %v", err)
	}
	if err := validateProblemProjectionMode(legacy, ""); err == nil {
		t.Fatal("legacy projection compatibility without an explicit development environment was accepted")
	}
	if err := validateProblemProjectionMode(legacy, "development"); err != nil {
		t.Fatalf("explicit development compatibility was rejected: %v", err)
	}
}
