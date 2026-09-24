// Runtime readiness checks retain the service-specific dependency requirements.
package svc

import (
	"context"
	"errors"
)

func (s *ServiceContext) Ready(ctx context.Context) error {
	if s == nil || s.ObjectStore == nil {
		return errors.New("object storage backend is unavailable")
	}
	return s.ObjectStore.Ready(ctx)
}
