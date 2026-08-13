package artifactgc

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"
)

const (
	DefaultRetention        = 7 * 24 * time.Hour
	MinimumRetention        = 24 * time.Hour
	MaxAutomaticFailures    = 3
	firstFailureRetryDelay  = time.Minute
	secondFailureRetryDelay = 5 * time.Minute
)

type Intent struct {
	URI          string
	Key          string
	SHA256       string
	SizeBytes    int64
	UpdatedAt    time.Time
	ClaimToken   string
	ClaimUntil   time.Time
	AttemptCount int
	FailureCount int
}

type Object struct {
	Key       string `json:"key"`
	SHA256    string `json:"sha256"`
	SizeBytes int64  `json:"size_bytes"`
	UpdatedAt string `json:"updated_at"`
}

// Ledger is the Problem-owned source of truth for remote uploads that have not
// been linked by an immutable revision + outbox transaction. Implementations
// must make Claim exclusive against new registrations for the lease duration.
// Renew must be token-CAS and must never revive an expired or stolen claim.
// Retry may postpone another attempt but must never shorten the active claim:
// a provider request can outlive the caller's timeout until the isolation lease.
type Ledger interface {
	Claim(context.Context, time.Time, time.Duration) (*Intent, error)
	ConfirmDeletable(context.Context, Intent) (bool, error)
	Renew(context.Context, Intent, time.Duration) error
	CompleteAbsent(context.Context, Intent) error
	CompleteDeleted(context.Context, Intent) error
	CompleteReferenced(context.Context, Intent) error
	Release(context.Context, Intent, time.Duration) error
	Retry(context.Context, Intent, FailureDetail, time.Duration) error
	Quarantine(context.Context, Intent, FailureDetail) error
}

type ObjectStore interface {
	Inspect(context.Context, Intent) (Object, bool, error)
	DeleteIfMatches(context.Context, Intent) error
}

type Collector struct {
	Ledger     Ledger
	Store      ObjectStore
	Retention  time.Duration
	ClaimLease time.Duration
	// DeleteTimeout is the effective storage.object.delete ApiBinding timeout. A zero
	// value uses the bound store timeout when available, then the release default.
	DeleteTimeout time.Duration
	Delete        bool
	Now           func() time.Time
	BatchSize     int
}

type Report struct {
	StartedAtUTC   time.Time `json:"started_at_utc"`
	FinishedAtUTC  time.Time `json:"finished_at_utc"`
	DryRun         bool      `json:"dry_run"`
	Scanned        int       `json:"scanned"`
	Missing        int       `json:"missing"`
	Referenced     int       `json:"referenced"`
	Candidates     []string  `json:"candidates"`
	Deleted        []string  `json:"deleted"`
	NeedsAttention []string  `json:"needs_attention,omitempty"`
	Errors         []string  `json:"errors,omitempty"`
}

func (c Collector) Run(ctx context.Context) (Report, error) {
	now := time.Now
	if c.Now != nil {
		now = c.Now
	}
	report := Report{StartedAtUTC: now().UTC(), DryRun: !c.Delete}
	if c.Ledger == nil || c.Store == nil {
		return report, errors.New("artifact GC ledger and bound object store are required")
	}
	retention := c.Retention
	if retention == 0 {
		retention = DefaultRetention
	}
	if retention < MinimumRetention {
		return report, fmt.Errorf("artifact GC retention %s is below safe minimum %s", retention, MinimumRetention)
	}
	deleteTimeout := c.DeleteTimeout
	if deleteTimeout == 0 {
		if source, ok := c.Store.(interface {
			DeleteBindingTimeout() (time.Duration, error)
		}); ok {
			var err error
			deleteTimeout, err = source.DeleteBindingTimeout()
			if err != nil {
				return report, err
			}
		}
	}
	timing, err := ResolveDeleteIsolationTiming(c.ClaimLease, deleteTimeout)
	if err != nil {
		return report, err
	}
	claimLease := timing.ClaimLease
	batchSize := c.BatchSize
	if batchSize <= 0 || batchSize > 500 {
		batchSize = 100
	}
	cutoff := report.StartedAtUTC.Add(-retention)
	for report.Scanned < batchSize {
		intent, err := c.Ledger.Claim(ctx, cutoff, claimLease)
		if err != nil {
			return finishReport(report, now), err
		}
		if intent == nil {
			break
		}
		report.Scanned++
		object, exists, err := c.Store.Inspect(ctx, *intent)
		if err != nil {
			c.recordFailure(ctx, &report, *intent, "inspect", err, isDeterministicProviderFailure(err))
			continue
		}
		if !exists {
			deletable, err := c.Ledger.ConfirmDeletable(ctx, *intent)
			if err != nil {
				c.recordFailure(ctx, &report, *intent, "final ledger check", err, false)
				continue
			}
			if !deletable {
				c.recordFailure(ctx, &report, *intent, "referenced object missing", errors.New("database reference exists but bound Storage proved the object absent"), true)
				continue
			}
			if err := c.Ledger.CompleteAbsent(ctx, *intent); err != nil {
				c.recordFailure(ctx, &report, *intent, "remove missing intent", err, false)
				continue
			}
			report.Missing++
			continue
		}
		if !strings.EqualFold(strings.TrimSpace(object.SHA256), intent.SHA256) || object.SizeBytes != intent.SizeBytes || object.Key != intent.Key {
			c.recordFailure(ctx, &report, *intent, "identity", errors.New("stored object SHA-256, size, or key differs from upload intent"), true)
			continue
		}
		deletable, err := c.Ledger.ConfirmDeletable(ctx, *intent)
		if err != nil {
			c.recordFailure(ctx, &report, *intent, "final ledger check", err, false)
			continue
		}
		if !deletable {
			// A committed Problem reference always wins. Complete is token-CAS,
			// so it cannot remove a freshly re-registered intent that stole an
			// expired claim.
			if err := c.Ledger.CompleteReferenced(ctx, *intent); err != nil {
				c.recordFailure(ctx, &report, *intent, "release referenced intent", err, errors.Is(err, ErrReferenceIdentityMismatch))
				continue
			}
			report.Referenced++
			continue
		}
		report.Candidates = append(report.Candidates, intent.URI)
		if !c.Delete {
			if err := c.Ledger.Release(ctx, *intent, time.Hour); err != nil {
				report.Errors = append(report.Errors, fmt.Sprintf("release dry-run claim %s: %v", intent.URI, err))
			}
			continue
		}
		// Inspect and the final reference check may consume part of the original
		// lease. Renew immediately before DELETE so the complete configured lease
		// protects the bounded provider request plus its isolation grace.
		if err := c.Ledger.Renew(ctx, *intent, claimLease); err != nil {
			c.recordFailure(ctx, &report, *intent, "renew delete isolation", err, errors.Is(err, ErrReferenceIdentityMismatch))
			continue
		}
		if err := c.Store.DeleteIfMatches(ctx, *intent); err != nil {
			c.recordFailure(ctx, &report, *intent, "conditional delete", err, isDeterministicProviderFailure(err))
			continue
		}
		if err := c.Ledger.CompleteDeleted(ctx, *intent); err != nil {
			c.recordFailure(ctx, &report, *intent, "complete delete", err, false)
			continue
		}
		report.Deleted = append(report.Deleted, intent.URI)
	}
	report = finishReport(report, now)
	if len(report.Errors) > 0 {
		return report, fmt.Errorf("artifact GC completed with %d fail-closed errors", len(report.Errors))
	}
	return report, nil
}

func (c Collector) recordFailure(ctx context.Context, report *Report, intent Intent, stage string, cause error, deterministic bool) {
	failure := classifyFailure(stage, cause, deterministic)
	report.Errors = append(report.Errors, fmt.Sprintf("%s %s: %v", stage, intent.URI, cause))
	if deterministic || intent.FailureCount+1 >= MaxAutomaticFailures {
		if err := c.Ledger.Quarantine(ctx, intent, failure); err != nil {
			report.Errors = append(report.Errors, fmt.Sprintf("persist needs-attention %s: %v", intent.URI, err))
			return
		}
		report.NeedsAttention = append(report.NeedsAttention, intent.URI)
		return
	}
	backoff := firstFailureRetryDelay
	if intent.FailureCount > 0 {
		backoff = secondFailureRetryDelay
	}
	if err := c.Ledger.Retry(ctx, intent, failure, backoff); err != nil {
		report.Errors = append(report.Errors, fmt.Sprintf("persist retry %s: %v", intent.URI, err))
	}
}

func finishReport(report Report, now func() time.Time) Report {
	report.FinishedAtUTC = now().UTC()
	return report
}
