// Gateway Contribution snapshots, route projections and reconciliation.
package svc

import (
	"context"
	"fmt"
	"strings"
	"time"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"

	"go.uber.org/zap"
)

func (s *ServiceContext) reloadContributionSnapshot(ctx context.Context) error {
	if s == nil || s.Orchestrator == nil || s.ServiceProxy == nil {
		return fmt.Errorf("contribution snapshot consumer is not configured")
	}
	snapshot, err := s.Orchestrator.ContributionSnapshot(ctx)
	if err != nil {
		s.recordContributionError(err)
		return err
	}
	s.contributionMu.Lock()
	unchanged := snapshot.Digest != "" && snapshot.Digest == s.contributionDigest
	pending := s.contributionPending
	s.contributionMu.Unlock()
	if unchanged {
		if pending == nil || pending.Digest != snapshot.Digest {
			return nil
		}
		return s.acknowledgeContributionSnapshot(ctx, *pending)
	}
	table, err := servicestatus.ContributionRouteTable(snapshot)
	if err != nil {
		s.recordContributionError(err)
		return err
	}
	if err := s.ServiceProxy.ApplyContributionSnapshot(table, snapshot); err != nil {
		s.recordContributionError(err)
		return err
	}
	s.contributionMu.Lock()
	s.contributionDigest = snapshot.Digest
	s.contributionPending = &snapshot
	s.contributionReady = true
	s.contributionError = ""
	s.contributionMu.Unlock()
	return s.acknowledgeContributionSnapshot(ctx, snapshot)
}

func (s *ServiceContext) acknowledgeContributionSnapshot(ctx context.Context, snapshot orchestratorsnapshot.ContributionSnapshot) error {
	if s.Orchestrator == nil || !s.Orchestrator.ContributionAcknowledgementsConfigured() {
		return nil
	}
	if err := s.Orchestrator.AcknowledgeContributionSnapshot(ctx, snapshot); err != nil {
		s.recordContributionError(err)
		return err
	}
	s.contributionMu.Lock()
	if s.contributionPending != nil && s.contributionPending.Digest == snapshot.Digest {
		s.contributionPending = nil
		s.contributionAcked = snapshot.Digest
		s.contributionError = ""
	}
	s.contributionMu.Unlock()
	return nil
}

func (s *ServiceContext) recordContributionError(err error) {
	if s == nil || err == nil {
		return
	}
	s.contributionMu.Lock()
	s.contributionError = err.Error()
	s.contributionMu.Unlock()
}

func (s *ServiceContext) startContributionSnapshotReconciler(interval time.Duration) {
	if s == nil || interval <= 0 {
		return
	}
	s.contributionMu.Lock()
	if s.contributionCancel != nil {
		s.contributionMu.Unlock()
		return
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	s.contributionCancel = cancel
	s.contributionDone = done
	s.contributionMu.Unlock()
	go func() {
		defer close(done)
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				if err := s.reloadContributionSnapshot(ctx); err != nil && s.Logger != nil {
					s.Logger.Warn("orchestrator contribution snapshot reconciliation failed; retaining active revision", zap.Error(err))
				}
			}
		}
	}()
}

func routeTableOptionsFromConfig(cfg config.ProxyConfig) servicestatus.RouteTableOptions {
	trusted := make(map[string]servicestatus.TrustedService)
	for _, item := range cfg.TrustedServices {
		if strings.TrimSpace(item.ServiceID) == "" {
			continue
		}
		trusted[item.ServiceID] = servicestatus.TrustedService{
			ServiceID:     item.ServiceID,
			UpstreamBase:  item.Target,
			StripPrefix:   item.StripPrefix,
			RewritePrefix: item.RewritePrefix,
			HealthCheckID: item.HealthCheckID,
		}
	}
	for _, route := range cfg.Routes {
		serviceID := inferServiceID(route.Target)
		if serviceID == "" {
			continue
		}
		if _, ok := trusted[serviceID]; ok {
			continue
		}
		trusted[serviceID] = servicestatus.TrustedService{
			ServiceID:     serviceID,
			UpstreamBase:  route.Target,
			StripPrefix:   route.StripPrefix,
			HealthCheckID: serviceID + "-health",
		}
	}
	return servicestatus.RouteTableOptions{
		TrustedServices: trusted,
	}
}

func filterServiceStatusesByKind(items []servicestatus.ServiceStatus, workers bool) []servicestatus.ServiceStatus {
	out := make([]servicestatus.ServiceStatus, 0, len(items))
	for _, item := range items {
		if (item.Kind == "worker") == workers {
			out = append(out, item)
		}
	}
	return out
}
