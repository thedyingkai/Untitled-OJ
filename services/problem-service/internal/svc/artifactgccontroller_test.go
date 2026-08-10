package svc

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"ojos-problem-service/internal/artifactgc"
)

type fakeArtifactGCOperatorLedger struct {
	page         artifactgc.IntentPage
	reconcile    artifactgc.OperatorActionResult
	retry        artifactgc.OperatorActionResult
	reconcileErr error
	retryErr     error
	recoveryDue  bool
}

func (l *fakeArtifactGCOperatorLedger) ListIntents(context.Context, string, string, int) (artifactgc.IntentPage, error) {
	return l.page, nil
}

func (l *fakeArtifactGCOperatorLedger) RecoveryDue(context.Context) (bool, error) {
	return l.recoveryDue, nil
}

func (l *fakeArtifactGCOperatorLedger) RequestReconcile(context.Context, string, string, int64, string, string, string) (artifactgc.OperatorActionResult, error) {
	return l.reconcile, l.reconcileErr
}

func (l *fakeArtifactGCOperatorLedger) RetryNeedsAttention(context.Context, string, int, string, string, string) (artifactgc.OperatorActionResult, error) {
	return l.retry, l.retryErr
}

func TestArtifactGCControllerSerializesConcurrentRuns(t *testing.T) {
	var active atomic.Int32
	var maximum atomic.Int32
	controller := newArtifactGCController(&fakeArtifactGCOperatorLedger{recoveryDue: true}, func(context.Context) (artifactgc.Report, error) {
		current := active.Add(1)
		for {
			previous := maximum.Load()
			if current <= previous || maximum.CompareAndSwap(previous, current) {
				break
			}
		}
		time.Sleep(10 * time.Millisecond)
		active.Add(-1)
		return artifactgc.Report{}, nil
	})

	var wait sync.WaitGroup
	for range 8 {
		wait.Add(1)
		go func() {
			defer wait.Done()
			if _, err := controller.RunOnce(t.Context()); err != nil {
				t.Errorf("RunOnce: %v", err)
			}
		}()
	}
	wait.Wait()
	if got := maximum.Load(); got != 1 {
		t.Fatalf("collector executions overlapped: maximum=%d", got)
	}
}

func TestArtifactGCControllerWakeIsBoundedAndFailedMutationDoesNotWake(t *testing.T) {
	ledger := &fakeArtifactGCOperatorLedger{reconcileErr: errors.New("persist failed")}
	controller := newArtifactGCController(ledger, func(context.Context) (artifactgc.Report, error) {
		return artifactgc.Report{}, nil
	})
	if _, err := controller.RequestReconcile(t.Context(), "uri", "sha", 1, "actor", "reason", "key"); err == nil {
		t.Fatal("failed durable mutation was accepted")
	}
	select {
	case <-controller.wake:
		t.Fatal("failed durable mutation woke the collector")
	default:
	}
	controller.Wake()
	controller.Wake()
	if got := len(controller.wake); got != 1 {
		t.Fatalf("wake channel was not coalesced: %d", got)
	}
}

func TestArtifactGCControllerRecoveryPollDoesNotNeedHTTPWake(t *testing.T) {
	runs := make(chan struct{}, 4)
	controller := newArtifactGCController(&fakeArtifactGCOperatorLedger{recoveryDue: true}, func(context.Context) (artifactgc.Report, error) {
		runs <- struct{}{}
		return artifactgc.Report{}, nil
	})
	controller.recoveryPoll = 10 * time.Millisecond
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	go controller.RunLoop(ctx, 24*time.Hour, nil)
	select {
	case <-runs: // startup recovery pass
	case <-time.After(time.Second):
		t.Fatal("startup recovery pass did not run")
	}
	select {
	case <-runs: // lease/retry recovery poll, with no Wake call
	case <-time.After(time.Second):
		t.Fatal("bounded recovery poll did not run")
	}
}

func TestArtifactGCControllerRecoveryPollSkipsFullSweepWhenNothingIsDue(t *testing.T) {
	runs := make(chan struct{}, 2)
	controller := newArtifactGCController(&fakeArtifactGCOperatorLedger{recoveryDue: false}, func(context.Context) (artifactgc.Report, error) {
		runs <- struct{}{}
		return artifactgc.Report{}, nil
	})
	controller.recoveryPoll = 10 * time.Millisecond
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	go controller.RunLoop(ctx, time.Hour, nil)
	select {
	case <-runs:
	case <-time.After(time.Second):
		t.Fatal("startup sweep did not run")
	}
	select {
	case <-runs:
		t.Fatal("empty recovery probe triggered a full collector sweep")
	case <-time.After(50 * time.Millisecond):
	}
}
