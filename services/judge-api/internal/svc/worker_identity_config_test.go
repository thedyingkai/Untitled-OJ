package svc

import (
	"testing"

	"ojos-judge-api/internal/config"
)

func TestProductionWorkerIdentityConfigurationFailsClosed(t *testing.T) {
	valid := config.Config{}
	if err := validateWorkerIdentityMode(valid, true, "production"); err != nil {
		t.Fatalf("valid workload-only production configuration was rejected: %v", err)
	}
	if err := validateWorkerIdentityMode(valid, false, "production"); err == nil {
		t.Fatal("production without a workload verifier must be rejected")
	}

	legacyFlag := valid
	legacyFlag.WorkloadIdentity.AllowLegacyWorkerToken = true
	if err := validateWorkerIdentityMode(legacyFlag, true, "production"); err == nil {
		t.Fatal("production legacy-token opt-in must be rejected")
	}

	legacySecret := valid
	legacySecret.WorkerAuth.Token = "shared-worker-token"
	if err := validateWorkerIdentityMode(legacySecret, true, "production"); err == nil {
		t.Fatal("production shared Worker token must be rejected even when legacy mode is false")
	}

	if err := validateWorkerIdentityMode(legacySecret, false, "development"); err != nil {
		t.Fatalf("explicit development compatibility was rejected: %v", err)
	}
}
