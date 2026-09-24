// Runtime readiness checks retain the service-specific dependency requirements.
package svc

import (
	"context"
	"errors"
)

func (s *ServiceContext) Ready(ctx context.Context) error {
	if s == nil || s.DB == nil {
		return errors.New("claimed Auth PostgreSQL database is unavailable")
	}
	if err := s.DB.Ping(ctx); err != nil {
		return errors.New("claimed Auth PostgreSQL database is unavailable")
	}
	if productionModeEnabled() {
		if s.WorkloadIssuer == nil || s.TopologyProjection == nil || s.ContributionProjection == nil {
			return errors.New("managed Auth control-plane projection is unavailable")
		}
	}
	return nil
}
