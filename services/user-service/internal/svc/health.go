// Runtime readiness checks retain the service-specific dependency requirements.
package svc

import (
	"context"
	"errors"

	sharedperm "ojos-shared/security/permission"
)

func (s *ServiceContext) Ready(ctx context.Context) error {
	if s == nil || s.DB == nil {
		return errors.New("claimed PostgreSQL database is unavailable")
	}
	if err := s.DB.Ping(ctx); err != nil {
		return errors.New("claimed PostgreSQL database is unavailable")
	}
	if s.Context == nil {
		if managedEnvironment() {
			return errors.New("managed service context is unavailable")
		}
		return nil
	}
	// Invalid or partial replacements retain the last-known-good snapshot.
	_ = s.Context.ReloadNow()
	snapshot, err := s.Context.Current(ctx)
	if err != nil {
		return errors.New("managed service context is unavailable")
	}
	if err := snapshot.RequireService("user-service"); err != nil {
		return errors.New("managed service identity is invalid")
	}
	binding, err := snapshot.Binding(permissionBindingName)
	if err != nil || binding.APIID != sharedperm.DefaultPermissionCheckApiID {
		return errors.New("required permission API binding is unavailable")
	}
	if _, err := snapshot.Client(); err != nil {
		return errors.New("required permission API client is unavailable")
	}
	if _, err := s.Context.Credential(ctx); err != nil {
		return errors.New("workload credential is unavailable")
	}
	return nil
}
