package repository

import (
	"reflect"
	"testing"
)

func TestNormalizeTaskIDsTrimsAndDeduplicatesStreamTaskIDs(t *testing.T) {
	got := normalizeTaskIDs([]string{" sub-42 ", "", "sub-42", "sub-43"})
	want := []string{"sub-42", "sub-43"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("unexpected normalized task ids: got %#v want %#v", got, want)
	}
}

func TestMatchesSavedTaskFailureAcceptsOnlyExactDuplicateLeaseTransition(t *testing.T) {
	retryable := taskFailureSnapshot{
		Status:       "PENDING",
		WorkerID:     "",
		LeaseVersion: 7,
		Message:      "download source failed",
	}
	if !matchesSavedTaskFailure(
		retryable,
		"worker-a",
		7,
		"PENDING",
		"download source failed",
		true,
	) {
		t.Fatal("exact retryable duplicate should be accepted after worker_id is cleared")
	}

	terminal := taskFailureSnapshot{
		Status:       "FAILED",
		WorkerID:     "worker-a",
		LeaseVersion: 9,
		Message:      "invalid package",
	}
	if !matchesSavedTaskFailure(
		terminal,
		"worker-a",
		9,
		"FAILED",
		"invalid package",
		false,
	) {
		t.Fatal("exact terminal duplicate should be accepted")
	}

	for name, snapshot := range map[string]taskFailureSnapshot{
		"new lease":       {Status: "PENDING", WorkerID: "worker-b", LeaseVersion: 8, Message: "download source failed"},
		"different state": {Status: "RUNNING", WorkerID: "worker-a", LeaseVersion: 7, Message: "download source failed"},
		"different error": {Status: "PENDING", WorkerID: "", LeaseVersion: 7, Message: "another failure"},
	} {
		t.Run(name, func(t *testing.T) {
			if matchesSavedTaskFailure(
				snapshot,
				"worker-a",
				7,
				"PENDING",
				"download source failed",
				true,
			) {
				t.Fatal("non-identical transition must remain a stale lease")
			}
		})
	}
}
