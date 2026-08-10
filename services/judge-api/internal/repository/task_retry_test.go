package repository

import (
	"bytes"
	"encoding/json"
	"testing"
	"time"
)

func TestTaskRetryPolicyUsesBoundedBackoffAndAttemptBudget(t *testing.T) {
	t.Parallel()

	for _, test := range []struct {
		attempt   int
		delay     time.Duration
		exhausted bool
	}{
		{attempt: 1, delay: time.Second},
		{attempt: 2, delay: 5 * time.Second},
		{attempt: 3, delay: 30 * time.Second},
		{attempt: 4, delay: 30 * time.Second, exhausted: true},
		{attempt: 5, delay: 30 * time.Second, exhausted: true},
	} {
		t.Run(test.delay.String(), func(t *testing.T) {
			if actual := taskRetryDelay(test.attempt); actual != test.delay {
				t.Fatalf("attempt %d delay: got %s want %s", test.attempt, actual, test.delay)
			}
			if actual := taskRetryExhausted(test.attempt); actual != test.exhausted {
				t.Fatalf("attempt %d exhausted: got %t want %t", test.attempt, actual, test.exhausted)
			}
		})
	}

	now := time.Date(2026, time.August, 9, 0, 0, 0, 0, time.UTC)
	if actual := taskRetryAvailableAt(now, 2); !actual.Equal(now.Add(5 * time.Second)) {
		t.Fatalf("available_at: got %s", actual)
	}
}

func TestExpiredTaskFailureTransitionIsDeterministicAndTerminal(t *testing.T) {
	t.Parallel()

	task := expiredTaskLease{
		TaskID:         "sub-42",
		SubmissionID:   42,
		WorkerID:       "worker-b",
		LeaseVersion:   4,
		Attempt:        4,
		LeaseExpiresAt: time.Date(2026, time.August, 9, 1, 2, 3, 4, time.UTC),
	}
	first, err := expiredTaskFailureTransition(task)
	if err != nil {
		t.Fatal(err)
	}
	second, err := expiredTaskFailureTransition(task)
	if err != nil {
		t.Fatal(err)
	}
	if first.Status != second.Status ||
		first.Message != second.Message ||
		first.Retryable != second.Retryable ||
		first.PayloadSHA256 != second.PayloadSHA256 ||
		first.OutboxEventID != second.OutboxEventID ||
		!bytes.Equal(first.OutboxPayload, second.OutboxPayload) {
		t.Fatalf("exhausted transition is not deterministic: %#v %#v", first, second)
	}
	if first.Retryable || first.Status != "SYSTEM_ERROR" || first.Message == "" {
		t.Fatalf("unexpected exhausted transition: %#v", first)
	}
	if len(first.PayloadSHA256) != 64 || first.OutboxEventID == "" || !json.Valid(first.OutboxPayload) {
		t.Fatalf("invalid exhausted transition identity: %#v", first)
	}
}
