// Runtime readiness checks retain the service-specific dependency requirements.
package svc

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"

	sharedperm "ojos-shared/security/permission"
)

func (s *ServiceContext) Ready(ctx context.Context) error {
	if s == nil || s.DB == nil || s.DB.Ping(ctx) != nil {
		return errors.New("claimed PostgreSQL database is unavailable")
	}
	if err := probeProblemsRoot(s.Config.Storage.ProblemsRoot); err != nil {
		return fmt.Errorf("problem package volume is unavailable: %w", err)
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
	if err != nil || snapshot.RequireService("problem-service") != nil {
		return errors.New("managed Service Context is invalid")
	}
	required := map[string]string{
		permissionBindingName: sharedperm.DefaultPermissionCheckApiID,
		storagePutBinding:     storagePutBinding,
		storageHeadBinding:    storageHeadBinding,
		storageDeleteBinding:  storageDeleteBinding,
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

// probeProblemsRoot exercises the exact primitives required by the durable
// mutation journal: a private file can be written and fsynced, atomically
// renamed within the volume, and the containing directory can be synced. It
// leaves no readiness artifact behind.
func probeProblemsRoot(root string) error {
	root = strings.TrimSpace(root)
	if root == "" || !filepath.IsAbs(root) {
		return errors.New("problems root must be an absolute path")
	}
	info, err := os.Lstat(root)
	if err != nil {
		return err
	}
	if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
		return errors.New("problems root is not a real directory")
	}
	temporary, err := os.CreateTemp(root, ".ojos-readiness-*.tmp")
	if err != nil {
		return err
	}
	temporaryPath := temporary.Name()
	renamedPath := temporaryPath + ".renamed"
	defer func() {
		_ = temporary.Close()
		_ = os.Remove(temporaryPath)
		_ = os.Remove(renamedPath)
	}()
	if err := temporary.Chmod(0o600); err != nil {
		return err
	}
	if _, err := temporary.WriteString("ready\n"); err != nil {
		return err
	}
	if err := temporary.Sync(); err != nil {
		return err
	}
	if err := temporary.Close(); err != nil {
		return err
	}
	if err := os.Rename(temporaryPath, renamedPath); err != nil {
		return err
	}
	if runtime.GOOS != "windows" {
		directory, err := os.Open(root)
		if err != nil {
			return err
		}
		syncErr := directory.Sync()
		closeErr := directory.Close()
		if err := errors.Join(syncErr, closeErr); err != nil {
			return err
		}
	}
	if err := os.Remove(renamedPath); err != nil {
		return err
	}
	return nil
}
