package svc

import (
	"strings"
	"testing"
	"time"

	"ojos-problem-service/internal/artifactgc"
	"ojos-shared/servicecontext"
)

func TestConfiguredArtifactGCDeleteTimingUsesReleaseDefaults(t *testing.T) {
	t.Setenv("OJOS_PROBLEM_ARTIFACT_GC_CLAIM_LEASE", "")
	store := boundStoreWithDeleteTimeout(60_000)
	timing, err := configuredArtifactGCDeleteTiming(store)
	if err != nil {
		t.Fatal(err)
	}
	if timing.ClaimLease != 10*time.Minute || timing.DeleteTimeout != 60*time.Second || timing.Grace != 60*time.Second {
		t.Fatalf("unexpected production GC timing: %#v", timing)
	}
}

func TestConfiguredArtifactGCDeleteTimingRejectsLegacyEnvironment(t *testing.T) {
	t.Setenv("OJOS_PROBLEM_ARTIFACT_GC_CLAIM_LEASE", "2m")
	_, err := configuredArtifactGCDeleteTiming(boundStoreWithDeleteTimeout(300_000))
	if err == nil || !strings.Contains(err.Error(), "must exceed") {
		t.Fatalf("legacy 120s lease / 300s delete timeout must fail closed, got %v", err)
	}
}

func boundStoreWithDeleteTimeout(timeoutMS uint64) *artifactgc.BoundObjectStore {
	return &artifactgc.BoundObjectStore{Context: servicecontext.ServiceContext{
		Bindings: map[string]servicecontext.APIBinding{
			"storage_delete": {
				BindingID: "delete",
				APIID:     "storage.object.delete",
				BasePath:  "/internal/apis/storage.object.delete",
				TimeoutMS: timeoutMS,
			},
		},
	}}
}
