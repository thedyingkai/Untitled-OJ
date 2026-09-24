// Runtime readiness checks retain the service-specific dependency requirements.
package svc

import (
	"context"
	"errors"
	"fmt"

	sharedperm "ojos-shared/security/permission"
)

func (s *ServiceContext) Ready(ctx context.Context) error {
	if s == nil || s.DB == nil || s.DB.Ping(ctx) != nil {
		return errors.New("claimed PostgreSQL database is unavailable")
	}
	if s.EventRedis == nil || s.EventRedis.Ping(ctx).Err() != nil {
		return errors.New("event transport is unavailable")
	}
	if s.Context == nil {
		if managedEnvironment() {
			return errors.New("managed Service Context is unavailable")
		}
		return nil
	}
	_ = s.Context.ReloadNow()
	snapshot, err := s.Context.Current(ctx)
	if err != nil || snapshot.RequireService("judge-api") != nil {
		return errors.New("managed Service Context is invalid")
	}
	required := map[string]string{
		permissionBindingName: sharedperm.DefaultPermissionCheckApiID,
		storageGetBinding:     storageGetBinding,
		storagePutBinding:     storagePutBinding,
		storageHeadBinding:    storageHeadBinding,
	}
	for name, apiID := range required {
		binding, bindingErr := snapshot.Binding(name)
		if bindingErr != nil || binding.APIID != apiID {
			return fmt.Errorf("required API binding %s is unavailable", name)
		}
	}
	if _, err := snapshot.Client(); err != nil {
		return errors.New("required API client is unavailable")
	}
	if _, err := s.Context.Credential(ctx); err != nil {
		return errors.New("workload credential is unavailable")
	}
	return nil
}
