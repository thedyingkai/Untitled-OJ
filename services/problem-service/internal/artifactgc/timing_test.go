package artifactgc

import (
	"strings"
	"testing"
	"time"
)

func TestDeleteIsolationTimingRejectsLegacyLeaseAndDeleteTimeout(t *testing.T) {
	_, err := ResolveDeleteIsolationTiming(2*time.Minute, 5*time.Minute)
	if err == nil || !strings.Contains(err.Error(), "must exceed") {
		t.Fatalf("legacy 120s lease / 300s delete timeout must fail closed, got %v", err)
	}
}

func TestDeleteIsolationTimingAcceptsReleaseDefaults(t *testing.T) {
	timing, err := ResolveDeleteIsolationTiming(10*time.Minute, 60*time.Second)
	if err != nil {
		t.Fatalf("release timing rejected: %v", err)
	}
	if timing.ClaimLease != 10*time.Minute || timing.DeleteTimeout != 60*time.Second || timing.Grace != 60*time.Second {
		t.Fatalf("unexpected release timing: %#v", timing)
	}
}

func TestDeleteIsolationTimingRequiresStrictGraceBoundary(t *testing.T) {
	_, err := ResolveDeleteIsolationTiming(2*time.Minute, 60*time.Second)
	if err == nil || !strings.Contains(err.Error(), "isolation grace") {
		t.Fatalf("claim lease equal to timeout plus grace must be rejected, got %v", err)
	}
}

func TestDeleteIsolationTimingDefaultsMatchRelease(t *testing.T) {
	timing, err := ResolveDeleteIsolationTiming(0, 0)
	if err != nil {
		t.Fatal(err)
	}
	if timing.ClaimLease != DefaultClaimLease || timing.DeleteTimeout != DefaultDeleteTimeout {
		t.Fatalf("resolved defaults do not match release defaults: %#v", timing)
	}
}
