package svc

import (
	"context"
	"net/http"
	"sync"
	"time"

	"ojos-problem-service/internal/artifactgc"
)

type artifactGCUnavailableError struct{}

func (artifactGCUnavailableError) Error() string         { return "artifact GC service unavailable" }
func (artifactGCUnavailableError) HTTPStatus() int       { return http.StatusServiceUnavailable }
func (artifactGCUnavailableError) ErrorCode() int        { return 50331 }
func (artifactGCUnavailableError) PublicMessage() string { return "artifact GC service unavailable" }

var ErrArtifactGCUnavailable error = artifactGCUnavailableError{}

const artifactGCRecoveryPoll = 30 * time.Second

// artifactGCOperatorLedger is the durable operator-facing part of the GC
// ledger. Provider I/O is deliberately absent: HTTP mutations only persist an
// action and wake the background controller.
type artifactGCOperatorLedger interface {
	ListIntents(context.Context, string, string, int) (artifactgc.IntentPage, error)
	RecoveryDue(context.Context) (bool, error)
	RequestReconcile(context.Context, string, string, int64, string, string, string) (artifactgc.OperatorActionResult, error)
	RetryNeedsAttention(context.Context, string, int, string, string, string) (artifactgc.OperatorActionResult, error)
}

// ArtifactGCController owns the single in-process execution lane for both the
// periodic collector and operator-triggered reconciliation. The one-slot wake
// channel coalesces bursts; durable manual markers in PostgreSQL make a lost
// process-local wake harmless across restarts.
type ArtifactGCController struct {
	ledger       artifactGCOperatorLedger
	run          func(context.Context) (artifactgc.Report, error)
	wake         chan struct{}
	runMu        sync.Mutex
	recoveryPoll time.Duration
}

func NewArtifactGCController(ledger artifactGCOperatorLedger, collector artifactgc.Collector) *ArtifactGCController {
	return newArtifactGCController(ledger, collector.Run)
}

func newArtifactGCController(
	ledger artifactGCOperatorLedger,
	run func(context.Context) (artifactgc.Report, error),
) *ArtifactGCController {
	return &ArtifactGCController{
		ledger:       ledger,
		run:          run,
		wake:         make(chan struct{}, 1),
		recoveryPoll: artifactGCRecoveryPoll,
	}
}

func (c *ArtifactGCController) ListIntents(
	ctx context.Context,
	status string,
	cursor string,
	limit int,
) (artifactgc.IntentPage, error) {
	if c == nil || c.ledger == nil {
		return artifactgc.IntentPage{}, ErrArtifactGCUnavailable
	}
	return c.ledger.ListIntents(ctx, status, cursor, limit)
}

func (c *ArtifactGCController) RequestReconcile(
	ctx context.Context,
	uri string,
	sha256 string,
	sizeBytes int64,
	actor string,
	reason string,
	idempotencyKey string,
) (artifactgc.OperatorActionResult, error) {
	if c == nil || c.ledger == nil {
		return artifactgc.OperatorActionResult{}, ErrArtifactGCUnavailable
	}
	result, err := c.ledger.RequestReconcile(ctx, uri, sha256, sizeBytes, actor, reason, idempotencyKey)
	if err != nil {
		return artifactgc.OperatorActionResult{}, err
	}
	c.Wake()
	return result, nil
}

func (c *ArtifactGCController) RetryNeedsAttention(
	ctx context.Context,
	uri string,
	expectedFailureCount int,
	actor string,
	reason string,
	idempotencyKey string,
) (artifactgc.OperatorActionResult, error) {
	if c == nil || c.ledger == nil {
		return artifactgc.OperatorActionResult{}, ErrArtifactGCUnavailable
	}
	result, err := c.ledger.RetryNeedsAttention(ctx, uri, expectedFailureCount, actor, reason, idempotencyKey)
	if err != nil {
		return artifactgc.OperatorActionResult{}, err
	}
	c.Wake()
	return result, nil
}

// Wake is non-blocking by design. At most one additional pass is useful while
// a pass is active, and the database marker remains authoritative if the
// process exits before consuming this signal.
func (c *ArtifactGCController) Wake() {
	if c == nil || c.wake == nil {
		return
	}
	select {
	case c.wake <- struct{}{}:
	default:
	}
}

// RunOnce serializes every collector pass. It is exported for deterministic
// lifecycle tests and internal readiness tooling, not exposed as an HTTP API.
func (c *ArtifactGCController) RunOnce(ctx context.Context) (artifactgc.Report, error) {
	if c == nil || c.run == nil {
		return artifactgc.Report{}, ErrArtifactGCUnavailable
	}
	c.runMu.Lock()
	defer c.runMu.Unlock()
	return c.run(ctx)
}

func (c *ArtifactGCController) RunLoop(
	ctx context.Context,
	interval time.Duration,
	observe func(artifactgc.Report, error),
) {
	poll := c.recoveryPoll
	if poll <= 0 {
		poll = artifactGCRecoveryPoll
	}
	if interval <= 0 {
		interval = 24 * time.Hour
	}
	observeRun := func() {
		report, err := c.RunOnce(ctx)
		if observe != nil && ctx.Err() == nil {
			observe(report, err)
		}
	}
	observeRun()
	sweep := time.NewTicker(interval)
	recovery := time.NewTicker(poll)
	defer sweep.Stop()
	defer recovery.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-c.wake:
			observeRun()
		case <-sweep.C:
			observeRun()
		case <-recovery.C:
			if c.ledger == nil {
				continue
			}
			due, err := c.ledger.RecoveryDue(ctx)
			if err != nil {
				if observe != nil && ctx.Err() == nil {
					observe(artifactgc.Report{}, err)
				}
				continue
			}
			if due {
				observeRun()
			}
		}
	}
}
