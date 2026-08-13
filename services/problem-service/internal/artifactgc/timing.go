package artifactgc

import (
	"fmt"
	"time"
)

const (
	DefaultClaimLease           = 10 * time.Minute
	MinimumClaimLease           = 30 * time.Second
	MaximumClaimLease           = 10 * time.Minute
	DefaultDeleteTimeout        = 60 * time.Second
	MinimumDeleteTimeout        = time.Millisecond
	MaximumDeleteTimeout        = 5 * time.Minute
	DeleteRequestIsolationGrace = 60 * time.Second
)

type DeleteIsolationTiming struct {
	ClaimLease    time.Duration
	DeleteTimeout time.Duration
	Grace         time.Duration
}

// ResolveDeleteIsolationTiming establishes the bounded interval during which
// an old conditional DELETE may still be executing. A DELETING ledger row is
// not reclaimable (and publishers cannot reuse its URI) for the whole request
// timeout plus an explicit transport/provider cancellation grace period.
func ResolveDeleteIsolationTiming(claimLease, deleteTimeout time.Duration) (DeleteIsolationTiming, error) {
	if claimLease == 0 {
		claimLease = DefaultClaimLease
	}
	if deleteTimeout == 0 {
		deleteTimeout = DefaultDeleteTimeout
	}
	result := DeleteIsolationTiming{
		ClaimLease:    claimLease,
		DeleteTimeout: deleteTimeout,
		Grace:         DeleteRequestIsolationGrace,
	}
	if claimLease < MinimumClaimLease || claimLease > MaximumClaimLease {
		return result, fmt.Errorf(
			"artifact GC claim lease %s is outside %s..%s",
			claimLease,
			MinimumClaimLease,
			MaximumClaimLease,
		)
	}
	if deleteTimeout < MinimumDeleteTimeout || deleteTimeout > MaximumDeleteTimeout {
		return result, fmt.Errorf(
			"artifact GC storage.object.delete timeout %s is outside %s..%s",
			deleteTimeout,
			MinimumDeleteTimeout,
			MaximumDeleteTimeout,
		)
	}
	minimumIsolation := deleteTimeout + DeleteRequestIsolationGrace
	if claimLease <= minimumIsolation {
		return result, fmt.Errorf(
			"artifact GC claim lease %s must exceed storage.object.delete timeout %s plus isolation grace %s",
			claimLease,
			deleteTimeout,
			DeleteRequestIsolationGrace,
		)
	}
	return result, nil
}
