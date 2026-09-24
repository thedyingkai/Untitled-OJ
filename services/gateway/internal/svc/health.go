// Runtime readiness checks retain the service-specific dependency requirements.
package svc

import (
	"context"
	"errors"
	"fmt"

	sharedperm "ojos-shared/security/permission"
)

func (s *ServiceContext) Ready(ctx context.Context) error {
	if s == nil || s.Redis == nil || s.Redis.Ping(ctx).Err() != nil {
		return errors.New("Gateway projection Redis is unavailable")
	}
	if s.Orchestrator == nil || !s.Orchestrator.Configured() {
		return errors.New("Orchestrator control-plane client is unavailable")
	}
	s.contributionMu.Lock()
	contributionReady := s.contributionReady
	contributionError := s.contributionError
	s.contributionMu.Unlock()
	if !contributionReady {
		if contributionError == "" {
			contributionError = "snapshot has not been observed"
		}
		return fmt.Errorf("active Contribution projection is unavailable: %s", contributionError)
	}
	if !managedEnvironment() {
		return nil
	}
	if s.Context == nil || s.PermissionChecker == nil {
		return errors.New("managed Service Context permission binding is unavailable")
	}
	_ = s.Context.ReloadNow()
	snapshot, err := s.Context.Current(ctx)
	if err != nil {
		return fmt.Errorf("read managed Service Context: %w", err)
	}
	if err := snapshot.RequireService("gateway"); err != nil {
		return err
	}
	binding, err := snapshot.Binding(permissionBindingName)
	if err != nil || binding.APIID != sharedperm.DefaultPermissionCheckApiID {
		return errors.New("managed auth.user.permission.check binding is unavailable")
	}
	if _, err := snapshot.Client(); err != nil {
		return fmt.Errorf("configure managed permission client: %w", err)
	}
	return nil
}
