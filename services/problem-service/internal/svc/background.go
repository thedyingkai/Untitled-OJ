// Service-owned background workers and their cancellation scope.
package svc

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"ojos-problem-service/internal/artifactgc"
	"ojos-problem-service/internal/projection"
	"ojos-shared/eventing"

	"go.uber.org/zap"
)

func (s *ServiceContext) startProjectionBackground() error {
	ctx, cancel := context.WithCancel(context.Background())
	s.backgroundCancel = cancel
	var transport eventing.TransportConfig
	if s.Events != nil {
		transport = s.Events.PublisherTransport()
	} else {
		// Compatibility is intentionally limited to unmanaged development.
		stream := strings.TrimSpace(os.Getenv("OJOS_PROBLEM_EVENT_STREAM"))
		if stream == "" {
			stream = eventing.DefaultEventStream
		}
		transport = eventing.DevelopmentPublisherTransport(stream)
	}
	relay, err := eventing.NewRelay(s.DB, s.EventRedis, transport)
	if err != nil {
		return fmt.Errorf("configure event relay failed: %w", err)
	}
	relay.RelayID = s.Config.Name
	relay.BatchSize = 100
	relay.LeaseDuration = 30 * time.Second
	relay.PollInterval = 250 * time.Millisecond
	if value := strings.TrimSpace(os.Getenv("OJOS_PROBLEM_EVENT_REPLAY_ON_START")); value == "1" || strings.EqualFold(value, "true") {
		if replayed, err := relay.ReplayPublished(ctx); err != nil {
			s.Logger.Warn("problem integration-event replay preparation failed", zap.Error(err))
		} else {
			s.Logger.Info("problem integration events scheduled for replay", zap.Int64("events", replayed))
		}
	}
	s.backgroundWG.Add(2)
	go func() {
		defer s.backgroundWG.Done()
		relay.Run(ctx)
	}()
	go func() {
		defer s.backgroundWG.Done()
		for {
			if _, err := projection.BackfillOnce(ctx, s.Repo, s.Config.Storage); err != nil && ctx.Err() == nil {
				s.Logger.Warn("problem projection backfill failed", zap.Error(err))
			}
			timer := time.NewTimer(5 * time.Minute)
			select {
			case <-ctx.Done():
				timer.Stop()
				return
			case <-timer.C:
			}
		}
	}()
	return s.startArtifactGC(ctx)
}

func (s *ServiceContext) startArtifactGC(ctx context.Context) error {
	production := strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production") || envBool("OJOS_MANAGED_WORKLOAD")
	if !envBoolDefault("OJOS_PROBLEM_ARTIFACT_GC_ENABLED", production) {
		return nil
	}
	retention, err := envDuration("OJOS_PROBLEM_ARTIFACT_GC_RETENTION", artifactgc.DefaultRetention)
	if err != nil || retention < artifactgc.MinimumRetention {
		if production {
			return fmt.Errorf("invalid production problem artifact GC retention %s: %v", retention, err)
		}
		s.Logger.Error("problem artifact GC disabled: invalid retention", zap.Duration("retention", retention), zap.Error(err))
		return nil
	}
	interval, err := envDuration("OJOS_PROBLEM_ARTIFACT_GC_INTERVAL", 24*time.Hour)
	if err != nil || interval < 5*time.Minute {
		if production {
			return fmt.Errorf("invalid production problem artifact GC interval %s: %v", interval, err)
		}
		s.Logger.Error("problem artifact GC disabled: invalid interval", zap.Duration("interval", interval), zap.Error(err))
		return nil
	}
	store, err := artifactgc.NewBoundObjectStore(bucketName(s.Config.Storage.Bucket))
	if err != nil {
		if production {
			return fmt.Errorf("configure production problem artifact GC ApiBindings failed: %w", err)
		}
		s.Logger.Error("problem artifact GC disabled: configure bound Storage", zap.Error(err))
		return nil
	}
	s.backgroundWG.Add(1)
	go func() {
		defer s.backgroundWG.Done()
		<-ctx.Done()
		_ = store.Close()
	}()
	timing, err := configuredArtifactGCDeleteTiming(store)
	if err != nil {
		if production {
			return fmt.Errorf("configure production problem artifact GC delete isolation failed: %w", err)
		}
		s.Logger.Error("problem artifact GC disabled: unsafe delete isolation timing", zap.Error(err))
		return nil
	}
	collector := artifactgc.Collector{
		Ledger:        artifactgc.PostgresLedger{DB: s.DB},
		Store:         store,
		Retention:     retention,
		ClaimLease:    timing.ClaimLease,
		DeleteTimeout: timing.DeleteTimeout,
		Delete:        envBoolDefault("OJOS_PROBLEM_ARTIFACT_GC_DELETE", production),
		BatchSize:     100,
	}
	controller := NewArtifactGCController(artifactgc.PostgresLedger{DB: s.DB}, collector)
	s.ArtifactGC = controller
	s.backgroundWG.Add(1)
	go func() {
		defer s.backgroundWG.Done()
		controller.RunLoop(ctx, interval, func(report artifactgc.Report, runErr error) {
			fields := []zap.Field{
				zap.Bool("dry_run", report.DryRun),
				zap.Int("scanned", report.Scanned),
				zap.Int("referenced", report.Referenced),
				zap.Int("candidates", len(report.Candidates)),
				zap.Int("deleted", len(report.Deleted)),
				zap.Duration("claim_lease", timing.ClaimLease),
				zap.Duration("delete_timeout", timing.DeleteTimeout),
				zap.Duration("delete_isolation_grace", timing.Grace),
			}
			if runErr != nil && ctx.Err() == nil {
				s.Logger.Warn("problem artifact GC scan failed closed", append(fields, zap.Error(runErr))...)
			} else if ctx.Err() == nil {
				s.Logger.Info("problem artifact GC scan completed", fields...)
			}
		})
	}()
	return nil
}

func configuredArtifactGCDeleteTiming(store *artifactgc.BoundObjectStore) (artifactgc.DeleteIsolationTiming, error) {
	if store == nil {
		return artifactgc.DeleteIsolationTiming{}, errors.New("artifact GC bound object store is required")
	}
	claimLease, err := envDuration("OJOS_PROBLEM_ARTIFACT_GC_CLAIM_LEASE", artifactgc.DefaultClaimLease)
	if err != nil {
		return artifactgc.DeleteIsolationTiming{}, fmt.Errorf("parse artifact GC claim lease: %w", err)
	}
	deleteTimeout, err := store.DeleteBindingTimeout()
	if err != nil {
		return artifactgc.DeleteIsolationTiming{}, err
	}
	return artifactgc.ResolveDeleteIsolationTiming(claimLease, deleteTimeout)
}

func bucketName(value string) string {
	if value = strings.TrimSpace(value); value != "" {
		return value
	}
	return "problems"
}
