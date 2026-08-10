package artifactgc

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

type fakeLedger struct {
	queue                 []Intent
	deletable             bool
	claimErr              error
	completeReferencedErr error
	renewErr              error
	completed             []string
	retried               []string
	released              []string
	quarantined           []string
	quarantineFailures    []FailureDetail
	renewed               []string
}

func (l *fakeLedger) Claim(_ context.Context, _ time.Time, _ time.Duration) (*Intent, error) {
	if l.claimErr != nil {
		return nil, l.claimErr
	}
	if len(l.queue) == 0 {
		return nil, nil
	}
	intent := l.queue[0]
	l.queue = l.queue[1:]
	return &intent, nil
}
func (l *fakeLedger) ConfirmDeletable(_ context.Context, _ Intent) (bool, error) {
	return l.deletable, nil
}
func (l *fakeLedger) Renew(_ context.Context, intent Intent, _ time.Duration) error {
	if l.renewErr != nil {
		return l.renewErr
	}
	l.renewed = append(l.renewed, intent.URI)
	return nil
}
func (l *fakeLedger) CompleteAbsent(_ context.Context, intent Intent) error {
	l.completed = append(l.completed, intent.URI)
	return nil
}
func (l *fakeLedger) CompleteDeleted(_ context.Context, intent Intent) error {
	l.completed = append(l.completed, intent.URI)
	return nil
}
func (l *fakeLedger) CompleteReferenced(_ context.Context, intent Intent) error {
	if l.completeReferencedErr != nil {
		return l.completeReferencedErr
	}
	l.completed = append(l.completed, intent.URI)
	return nil
}
func (l *fakeLedger) Retry(_ context.Context, intent Intent, _ FailureDetail, _ time.Duration) error {
	l.retried = append(l.retried, intent.URI)
	return nil
}
func (l *fakeLedger) Release(_ context.Context, intent Intent, _ time.Duration) error {
	l.released = append(l.released, intent.URI)
	return nil
}
func (l *fakeLedger) Quarantine(_ context.Context, intent Intent, failure FailureDetail) error {
	l.quarantined = append(l.quarantined, intent.URI)
	l.quarantineFailures = append(l.quarantineFailures, failure)
	return nil
}

type fakeStore struct {
	object       Object
	exists       bool
	inspectErr   error
	deleteErr    error
	inspectCalls int
	deleted      []string
}

func (s *fakeStore) Inspect(_ context.Context, _ Intent) (Object, bool, error) {
	s.inspectCalls++
	return s.object, s.exists, s.inspectErr
}
func (s *fakeStore) DeleteIfMatches(_ context.Context, intent Intent) error {
	if s.deleteErr != nil {
		return s.deleteErr
	}
	s.deleted = append(s.deleted, intent.URI)
	return nil
}

func orphanIntent(digest string) Intent {
	key := "package-sha256-" + digest + ".zip"
	return Intent{
		URI: "storage://problems/" + key, Key: key, SHA256: digest,
		SizeBytes: 17, ClaimToken: "claim", AttemptCount: 1,
	}
}

func TestCollectorDeletesOnlyLedgerOwnedMatchingOrphan(t *testing.T) {
	intent := orphanIntent(strings.Repeat("a", 64))
	ledger := &fakeLedger{queue: []Intent{intent}, deletable: true}
	store := &fakeStore{object: Object{Key: intent.Key, SHA256: intent.SHA256, SizeBytes: intent.SizeBytes}, exists: true}
	report, err := (Collector{Ledger: ledger, Store: store, Delete: true}).Run(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if len(report.Deleted) != 1 || len(store.deleted) != 1 || len(ledger.completed) != 1 || len(ledger.renewed) != 1 {
		t.Fatalf("orphan was not conditionally deleted and completed: report=%#v store=%#v ledger=%#v", report, store, ledger)
	}
}

func TestCollectorNeverDeletesWhenPreDeleteRenewalLosesClaim(t *testing.T) {
	intent := orphanIntent(strings.Repeat("6", 64))
	ledger := &fakeLedger{queue: []Intent{intent}, deletable: true, renewErr: ErrClaimLost}
	store := &fakeStore{object: Object{Key: intent.Key, SHA256: intent.SHA256, SizeBytes: intent.SizeBytes}, exists: true}
	report, err := (Collector{Ledger: ledger, Store: store, Delete: true}).Run(t.Context())
	if err == nil || len(store.deleted) != 0 || len(report.Errors) == 0 {
		t.Fatalf("lost pre-delete renewal did not fail closed: err=%v report=%#v store=%#v", err, report, store)
	}
}

func TestCollectorNeverDeletesWhenFinalProblemReferenceWins(t *testing.T) {
	intent := orphanIntent(strings.Repeat("b", 64))
	ledger := &fakeLedger{queue: []Intent{intent}, deletable: false}
	store := &fakeStore{object: Object{Key: intent.Key, SHA256: intent.SHA256, SizeBytes: intent.SizeBytes}, exists: true}
	report, err := (Collector{Ledger: ledger, Store: store, Delete: true}).Run(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if report.Referenced != 1 || len(store.deleted) != 0 || store.inspectCalls != 1 {
		t.Fatalf("committed reference did not win: report=%#v deleted=%#v", report, store.deleted)
	}
}

func TestCollectorRetainsLedgerWhenReferencedObjectIsMissing(t *testing.T) {
	intent := orphanIntent(strings.Repeat("9", 64))
	ledger := &fakeLedger{queue: []Intent{intent}, deletable: false}
	store := &fakeStore{}
	report, err := (Collector{Ledger: ledger, Store: store, Delete: true}).Run(t.Context())
	if err == nil || !strings.Contains(strings.Join(report.Errors, " "), "referenced object missing") ||
		len(ledger.completed) != 0 || len(ledger.retried) != 0 || len(ledger.quarantined) != 1 ||
		len(report.NeedsAttention) != 1 || len(store.deleted) != 0 {
		t.Fatalf("referenced missing object did not remain fail-closed: err=%v report=%#v ledger=%#v", err, report, ledger)
	}
}

func TestCollectorDoesNotMisreportLostLeaseAsReference(t *testing.T) {
	intent := orphanIntent(strings.Repeat("f", 64))
	ledger := &fakeLedger{
		queue: []Intent{intent}, deletable: false,
		completeReferencedErr: ErrClaimLost,
	}
	store := &fakeStore{object: Object{Key: intent.Key, SHA256: intent.SHA256, SizeBytes: intent.SizeBytes}, exists: true}
	report, err := (Collector{Ledger: ledger, Store: store, Delete: true}).Run(t.Context())
	if err == nil || report.Referenced != 0 || len(store.deleted) != 0 {
		t.Fatalf("lost claim was misreported as a committed reference: err=%v report=%#v", err, report)
	}
}

func TestCollectorQuarantinesReferenceIdentityMismatchImmediately(t *testing.T) {
	intent := orphanIntent(strings.Repeat("4", 64))
	ledger := &fakeLedger{
		queue:                 []Intent{intent},
		deletable:             false,
		completeReferencedErr: ErrReferenceIdentityMismatch,
	}
	store := &fakeStore{object: Object{Key: intent.Key, SHA256: intent.SHA256, SizeBytes: intent.SizeBytes}, exists: true}
	report, err := (Collector{Ledger: ledger, Store: store, Delete: true}).Run(t.Context())
	if err == nil || len(ledger.retried) != 0 || len(ledger.quarantined) != 1 ||
		len(report.NeedsAttention) != 1 || len(store.deleted) != 0 {
		t.Fatalf("reference identity mismatch did not terminate immediately: err=%v report=%#v ledger=%#v", err, report, ledger)
	}
}

func TestCollectorQuarantinesIdentityMismatchImmediately(t *testing.T) {
	intent := orphanIntent(strings.Repeat("c", 64))
	ledger := &fakeLedger{queue: []Intent{intent}, deletable: true}
	store := &fakeStore{object: Object{Key: intent.Key, SHA256: strings.Repeat("d", 64), SizeBytes: intent.SizeBytes}, exists: true}
	report, err := (Collector{Ledger: ledger, Store: store, Delete: true}).Run(t.Context())
	if err == nil || len(report.Errors) == 0 || len(store.deleted) != 0 || len(ledger.retried) != 0 ||
		len(ledger.quarantined) != 1 || len(report.NeedsAttention) != 1 {
		t.Fatalf("identity mismatch must require attention without deletion: err=%v report=%#v ledger=%#v", err, report, ledger)
	}
}

func TestCollectorQuarantinesThirdTransientFailure(t *testing.T) {
	digest := strings.Repeat("7", 64)
	intents := []Intent{orphanIntent(digest), orphanIntent(digest), orphanIntent(digest)}
	intents[0].FailureCount = 0
	intents[1].FailureCount = 1
	intents[2].FailureCount = 2
	ledger := &fakeLedger{queue: intents}
	store := &fakeStore{inspectErr: errors.New("dial tcp: connection refused")}
	report, err := (Collector{Ledger: ledger, Store: store, Delete: true, BatchSize: 3}).Run(t.Context())
	if err == nil || len(ledger.retried) != 2 || len(ledger.quarantined) != 1 ||
		len(report.NeedsAttention) != 1 || store.inspectCalls != 3 {
		t.Fatalf("transient failures did not stop after three attempts: err=%v report=%#v ledger=%#v", err, report, ledger)
	}
}

func TestCollectorQuarantinesConditionalPreconditionImmediately(t *testing.T) {
	intent := orphanIntent(strings.Repeat("8", 64))
	ledger := &fakeLedger{queue: []Intent{intent}, deletable: true}
	store := &fakeStore{
		object: Object{Key: intent.Key, SHA256: intent.SHA256, SizeBytes: intent.SizeBytes},
		exists: true,
		deleteErr: NewProviderHTTPError(
			"bound conditional Storage DELETE", 412, "object identity precondition failed",
		),
	}
	report, err := (Collector{Ledger: ledger, Store: store, Delete: true}).Run(t.Context())
	if err == nil || len(ledger.retried) != 0 || len(ledger.quarantined) != 1 ||
		len(report.NeedsAttention) != 1 || len(store.deleted) != 0 {
		t.Fatalf("conditional precondition did not terminate immediately: err=%v report=%#v ledger=%#v", err, report, ledger)
	}
}

func TestProviderFailureClassification(t *testing.T) {
	for _, status := range []int{400, 401, 403, 404, 405, 409, 410, 412, 422} {
		if !isDeterministicProviderFailure(NewProviderHTTPError("provider", status, "failure")) {
			t.Fatalf("HTTP %d was not deterministic", status)
		}
	}
	for _, status := range []int{408, 425, 429, 500, 502, 503, 504} {
		if isDeterministicProviderFailure(NewProviderHTTPError("provider", status, "failure")) {
			t.Fatalf("HTTP %d was not transient", status)
		}
	}
}

func TestCollectorDryRunReleasesWithoutRecordingFailure(t *testing.T) {
	intent := orphanIntent(strings.Repeat("5", 64))
	ledger := &fakeLedger{queue: []Intent{intent}, deletable: true}
	store := &fakeStore{object: Object{Key: intent.Key, SHA256: intent.SHA256, SizeBytes: intent.SizeBytes}, exists: true}
	report, err := (Collector{Ledger: ledger, Store: store, Delete: false}).Run(t.Context())
	if err != nil || len(ledger.released) != 1 || len(ledger.retried) != 0 || len(ledger.quarantined) != 0 {
		t.Fatalf("dry run was recorded as a failure: err=%v report=%#v ledger=%#v", err, report, ledger)
	}
}

func TestCollectorCompletesIntentWhenUploadNeverCreatedObject(t *testing.T) {
	intent := orphanIntent(strings.Repeat("e", 64))
	ledger := &fakeLedger{queue: []Intent{intent}, deletable: true}
	report, err := (Collector{Ledger: ledger, Store: &fakeStore{}, Delete: true}).Run(t.Context())
	if err != nil || report.Missing != 1 || len(ledger.completed) != 1 {
		t.Fatalf("missing upload intent was not reconciled: err=%v report=%#v", err, report)
	}
}

func TestCollectorDeletesMatchingZeroByteContentObject(t *testing.T) {
	digest := "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
	intent := Intent{
		URI: "storage://problems/problem-17-objects-sha256-" + digest,
		Key: "problem-17-objects-sha256-" + digest, SHA256: digest,
		SizeBytes: 0, ClaimToken: "claim", AttemptCount: 1,
	}
	ledger := &fakeLedger{queue: []Intent{intent}, deletable: true}
	store := &fakeStore{object: Object{Key: intent.Key, SHA256: digest, SizeBytes: 0}, exists: true}
	report, err := (Collector{Ledger: ledger, Store: store, Delete: true}).Run(t.Context())
	if err != nil || len(report.Deleted) != 1 || len(store.deleted) != 1 || len(ledger.completed) != 1 {
		t.Fatalf("matching zero-byte content object was not collected: err=%v report=%#v", err, report)
	}
}

func TestCollectorRejectsUnsafeRetentionAndLedgerFailure(t *testing.T) {
	_, err := (Collector{Ledger: &fakeLedger{}, Store: &fakeStore{}, Retention: time.Hour}).Run(t.Context())
	if err == nil || !strings.Contains(err.Error(), "safe minimum") {
		t.Fatalf("expected retention failure, got %v", err)
	}
	_, err = (Collector{Ledger: &fakeLedger{claimErr: errors.New("postgres unavailable")}, Store: &fakeStore{}}).Run(t.Context())
	if err == nil || !strings.Contains(err.Error(), "postgres unavailable") {
		t.Fatalf("expected ledger failure, got %v", err)
	}
}

func TestCollectorRejectsUnsafeDeleteIsolationBeforeClaim(t *testing.T) {
	ledger := &fakeLedger{}
	store := &fakeStore{}
	_, err := (Collector{
		Ledger: ledger, Store: store,
		ClaimLease: 2 * time.Minute, DeleteTimeout: 5 * time.Minute,
	}).Run(t.Context())
	if err == nil || !strings.Contains(err.Error(), "must exceed") || store.inspectCalls != 0 {
		t.Fatalf("unsafe delete isolation did not fail before provider access: err=%v store=%#v", err, store)
	}
}
