package svc

import (
	"testing"

	"ojos-gateway/internal/config"
)

func TestProductionGatewayRequiresWorkloadVerificationKey(t *testing.T) {
	if err := validateWorkloadIdentityConfig(config.WorkloadIdentityConfig{}, true); err == nil {
		t.Fatal("production Gateway accepted a missing workload public key")
	}
	if err := validateWorkloadIdentityConfig(config.WorkloadIdentityConfig{
		PublicKeyFile: "/run/secrets/workload-public.pem",
	}, true); err != nil {
		t.Fatalf("production Gateway rejected its workload public key: %v", err)
	}
	if err := validateWorkloadIdentityConfig(config.WorkloadIdentityConfig{}, false); err != nil {
		t.Fatalf("development Gateway may explicitly disable workload identity: %v", err)
	}
}
