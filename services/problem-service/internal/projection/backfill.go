package projection

import (
	"context"
	"strings"

	"ojos-problem-service/internal/config"
	"ojos-problem-service/internal/packagemutation"
	"ojos-problem-service/internal/repository"
	problemstorage "ojos-problem-service/internal/storage"
)

// BackfillOnce emits the same immutable full snapshot as online mutations.
// It is safe to restart: rows with a current outbox version are not selected,
// and immutable artifact publication is digest-idempotent.
func BackfillOnce(ctx context.Context, repo *repository.Repository, storage config.StorageConfig) (int, error) {
	if repo == nil {
		return 0, nil
	}
	afterID := int64(0)
	processed := 0
	for {
		candidates, err := repo.ListProjectionBackfillCandidates(ctx, afterID, 100)
		if err != nil {
			return processed, err
		}
		if len(candidates) == 0 {
			return processed, nil
		}
		for _, candidate := range candidates {
			published, err := backfillCandidate(ctx, repo, storage, candidate.ID)
			if err != nil {
				return processed, err
			}
			if published {
				processed++
			}
			afterID = candidate.ID
		}
	}
}

func backfillCandidate(ctx context.Context, repo *repository.Repository, storage config.StorageConfig, problemID int64) (bool, error) {
	session, err := repo.LockProblemMutation(ctx, problemID)
	if err != nil {
		return false, err
	}
	defer func() { _ = session.Close() }()
	lockedRepo := session.Repository()
	if err := packagemutation.RecoverProblemLocked(ctx, lockedRepo, storage.ProblemsRoot, problemID); err != nil {
		return false, err
	}
	problem, err := lockedRepo.GetProblem(ctx, problemID)
	if err != nil {
		return false, err
	}
	state, err := lockedRepo.ProblemProjectionState(ctx, problemID)
	if err != nil {
		return false, err
	}
	artifact, err := problemstorage.PublishPackageArtifactTracked(ctx, storage, problemID, problem.PackageDir, lockedRepo)
	if err != nil {
		return false, err
	}
	if state.HasCurrentOutbox && strings.EqualFold(strings.TrimSpace(state.PackageArtifactSHA256), artifact.SHA256) {
		// This is the explicit expand-first compatibility path for rows that may
		// predate the durable upload-intent ledger. Online mutations never use
		// this exemption and fail closed when their exact PENDING intent is absent.
		if err := lockedRepo.ResolveLegacyArtifactUploadIntent(ctx, artifact); err != nil {
			return false, err
		}
		return false, nil
	}
	if err := session.InTransaction(ctx, func(txRepo *repository.Repository) error {
		_, err := txRepo.PublishProblemSnapshotCAS(ctx, problemID, state.AggregateVersion, artifact)
		return err
	}); err != nil {
		return false, err
	}
	return true, nil
}
