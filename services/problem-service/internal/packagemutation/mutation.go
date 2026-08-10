package packagemutation

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"ojos-problem-service/internal/config"
	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/repository"
	problemstorage "ojos-problem-service/internal/storage"
	"ojos-shared/eventing"

	"github.com/jackc/pgx/v5"
)

const journalSchemaVersion = 1

var ErrRecoveryNeedsAttention = errors.New("problem package mutation recovery needs attention")

// Change describes the filesystem-derived state that must be committed in the
// same transaction as the matching immutable package snapshot.
type Change struct {
	Files               []packagefs.IndexedFile
	DeletedLogicalPaths []string
	ManifestSHA256      string
	Value               any
}

type MutateFunc func(problem *repository.Problem, stagingDir string) (Change, error)
type PersistFunc func(txRepo *repository.Repository, problem *repository.Problem, change Change) error
type CreatePersistFunc func(txRepo *repository.Repository, result *packagefs.CreateProblemResult) error
type DeletePersistFunc func(txRepo *repository.Repository, problem *repository.Problem, expectedAggregateVersion int64) error

// RunExisting serializes and publishes one existing problem package mutation.
// Slow filesystem, object-storage and hashing work happens outside the final DB
// transaction, but under the cross-process PostgreSQL session lock.
func RunExisting(
	ctx context.Context,
	repo *repository.Repository,
	storage config.StorageConfig,
	problemID int64,
	mutate MutateFunc,
	persist PersistFunc,
) (Change, error) {
	if repo == nil || mutate == nil || persist == nil {
		return Change{}, errors.New("problem package mutation is not configured")
	}
	session, err := repo.LockProblemMutation(ctx, problemID)
	if err != nil {
		return Change{}, err
	}
	defer func() { _ = session.Close() }()

	lockedRepo := session.Repository()
	if err := RecoverProblemLocked(ctx, lockedRepo, storage.ProblemsRoot, problemID); err != nil {
		return Change{}, err
	}
	problem, err := lockedRepo.GetProblem(ctx, problemID)
	if err != nil {
		return Change{}, err
	}
	state, err := lockedRepo.ProblemProjectionState(ctx, problemID)
	if err != nil {
		return Change{}, err
	}

	workspace, err := newReplaceWorkspace(storage.ProblemsRoot, problemID, problem.PackageDir, state.AggregateVersion)
	if err != nil {
		return Change{}, err
	}
	if err := workspace.cloneLiveTree(); err != nil {
		return Change{}, err
	}
	abortStaging := true
	defer func() {
		if abortStaging {
			_ = workspace.abortStaging()
		}
	}()

	change, err := mutate(problem, workspace.stagingDir)
	if err != nil {
		return Change{}, err
	}
	if err := validateStagedPackage(workspace.stagingDir); err != nil {
		return Change{}, err
	}
	if err := syncTree(workspace.stagingDir); err != nil {
		return Change{}, err
	}
	change.Files, err = problemstorage.SyncProblemFiles(ctx, storage, problemID, change.Files, lockedRepo)
	if err != nil {
		return Change{}, err
	}
	change.Files, err = rebaseLocalFiles(change.Files, workspace.stagingDir, workspace.liveDir)
	if err != nil {
		return Change{}, err
	}
	artifact, err := problemstorage.PublishPackageArtifactTracked(ctx, storage, problemID, workspace.stagingDir, lockedRepo)
	if err != nil {
		return Change{}, err
	}

	err = session.InTransaction(ctx, func(txRepo *repository.Repository) error {
		if err := persist(txRepo, problem, change); err != nil {
			return err
		}
		if err := txRepo.ResolveProblemFileUploadIntents(ctx, change.Files); err != nil {
			return err
		}
		if _, err := txRepo.PublishProblemSnapshotCAS(ctx, problemID, state.AggregateVersion, artifact); err != nil {
			return err
		}
		return workspace.publishBeforeCommit(artifact, state.AggregateVersion+1)
	})
	if err != nil {
		// InTransaction has rolled back before returning. If the directory swap
		// started, restore the byte-exact backup while the same lock is held.
		if workspace.journalWritten {
			recoveryCtx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			outcome, recoverErr := recoverAfterTransactionError(recoveryCtx, repo, session, workspace, artifact.SHA256)
			cancel()
			if recoverErr != nil {
				return Change{}, errors.Join(err, recoverErr)
			}
			// A transport failure while COMMIT is in flight is ambiguous. Recovery
			// reads the durable DB version while the mutation lock is still held;
			// when it proves that the transaction committed, report success so a
			// client retry cannot publish the same logical mutation twice.
			if outcome == recoveryCommitted {
				abortStaging = false
				return change, nil
			}
		}
		return Change{}, err
	}

	abortStaging = false
	// A cleanup failure is recoverable and must not turn a committed mutation
	// into an apparent failure that a client would replay. The durable journal
	// is intentionally retained and startup/next mutation will finish cleanup.
	_ = workspace.completeCommitted()
	return change, nil
}

// RunCreate applies the same journal protocol to initial package publication.
// The reserved problem ID is locked even before its row exists, so a retried
// create and startup recovery cannot publish two authoring trees.
func RunCreate(
	ctx context.Context,
	repo *repository.Repository,
	storage config.StorageConfig,
	args packagefs.CreateProblemArgs,
	persist CreatePersistFunc,
) (*packagefs.CreateProblemResult, error) {
	if repo == nil || persist == nil || args.ID <= 0 {
		return nil, errors.New("problem package create mutation is not configured")
	}
	session, err := repo.LockProblemMutation(ctx, args.ID)
	if err != nil {
		return nil, err
	}
	defer func() { _ = session.Close() }()
	lockedRepo := session.Repository()
	if err := RecoverProblemLocked(ctx, lockedRepo, storage.ProblemsRoot, args.ID); err != nil {
		return nil, err
	}
	if _, err := lockedRepo.ProblemProjectionState(ctx, args.ID); err == nil {
		return nil, fmt.Errorf("problem already exists: %d", args.ID)
	} else if !errors.Is(err, pgx.ErrNoRows) {
		return nil, err
	}

	root, err := cleanRoot(storage.ProblemsRoot)
	if err != nil {
		return nil, err
	}
	temporaryRoot := filepath.Join(root, fmt.Sprintf(".problem-%d.create-root", args.ID))
	if err := os.RemoveAll(temporaryRoot); err != nil {
		return nil, err
	}
	if err := os.Mkdir(temporaryRoot, 0o700); err != nil {
		return nil, err
	}
	defer os.RemoveAll(temporaryRoot)
	args.Root = temporaryRoot
	result, err := packagefs.CreateInitialPackage(args)
	if err != nil {
		return nil, err
	}
	liveDir := filepath.Join(root, filepath.Base(result.PackageDir))
	workspace, err := newCreateWorkspace(root, args.ID, liveDir)
	if err != nil {
		return nil, err
	}
	if _, err := os.Stat(workspace.liveDir); err == nil {
		return nil, fmt.Errorf("problem package directory already exists: %s", workspace.liveDir)
	} else if !os.IsNotExist(err) {
		return nil, err
	}
	if err := os.RemoveAll(workspace.stagingDir); err != nil {
		return nil, err
	}
	createdDir := result.PackageDir
	if err := os.Rename(createdDir, workspace.stagingDir); err != nil {
		return nil, err
	}
	result.Files, err = rebaseLocalFiles(result.Files, createdDir, workspace.stagingDir)
	if err != nil {
		_ = workspace.abortStaging()
		return nil, err
	}
	result.PackageDir = workspace.liveDir
	if err := validateStagedPackage(workspace.stagingDir); err != nil {
		_ = workspace.abortStaging()
		return nil, err
	}
	if err := syncTree(workspace.stagingDir); err != nil {
		_ = workspace.abortStaging()
		return nil, err
	}
	result.Files, err = problemstorage.SyncProblemFiles(ctx, storage, args.ID, result.Files, lockedRepo)
	if err != nil {
		_ = workspace.abortStaging()
		return nil, err
	}
	result.Files, err = rebaseLocalFiles(result.Files, workspace.stagingDir, workspace.liveDir)
	if err != nil {
		_ = workspace.abortStaging()
		return nil, err
	}
	artifact, err := problemstorage.PublishPackageArtifactTracked(ctx, storage, args.ID, workspace.stagingDir, lockedRepo)
	if err != nil {
		_ = workspace.abortStaging()
		return nil, err
	}

	err = session.InTransaction(ctx, func(txRepo *repository.Repository) error {
		if err := persist(txRepo, result); err != nil {
			return err
		}
		if err := txRepo.ResolveProblemFileUploadIntents(ctx, result.Files); err != nil {
			return err
		}
		if _, err := txRepo.PublishProblemSnapshotCAS(ctx, args.ID, 0, artifact); err != nil {
			return err
		}
		return workspace.publishBeforeCommit(artifact, 1)
	})
	if err != nil {
		if workspace.journalWritten {
			recoveryCtx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			outcome, recoverErr := recoverAfterTransactionError(recoveryCtx, repo, session, workspace, artifact.SHA256)
			cancel()
			if recoverErr != nil {
				return nil, errors.Join(err, recoverErr)
			}
			if outcome == recoveryCommitted {
				return result, nil
			}
		} else {
			_ = workspace.abortStaging()
		}
		return nil, err
	}
	_ = workspace.completeCommitted()
	return result, nil
}

// RunDelete makes the package-directory removal and tombstone transaction one
// recoverable state transition. A failed DB transaction restores the renamed
// directory; a committed tombstone removes the backup during cleanup/recovery.
func RunDelete(
	ctx context.Context,
	repo *repository.Repository,
	root string,
	problemID int64,
	persist DeletePersistFunc,
) error {
	if repo == nil || persist == nil || problemID <= 0 {
		return errors.New("problem package delete mutation is not configured")
	}
	session, err := repo.LockProblemMutation(ctx, problemID)
	if err != nil {
		return err
	}
	defer func() { _ = session.Close() }()
	lockedRepo := session.Repository()
	if err := RecoverProblemLocked(ctx, lockedRepo, root, problemID); err != nil {
		return err
	}
	problem, err := lockedRepo.GetProblem(ctx, problemID)
	if err != nil {
		return err
	}
	state, err := lockedRepo.ProblemProjectionState(ctx, problemID)
	if err != nil {
		return err
	}
	workspace, err := newDeleteWorkspace(root, problemID, problem.PackageDir, state.AggregateVersion)
	if err != nil {
		return err
	}
	err = session.InTransaction(ctx, func(txRepo *repository.Repository) error {
		if err := persist(txRepo, problem, state.AggregateVersion); err != nil {
			return err
		}
		return workspace.publishDeleteBeforeCommit(state.PackageArtifactSHA256, state.AggregateVersion+1)
	})
	if err != nil {
		if workspace.journalWritten {
			recoveryCtx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			outcome, recoverErr := recoverAfterTransactionError(recoveryCtx, repo, session, workspace, state.PackageArtifactSHA256)
			cancel()
			if recoverErr != nil {
				return errors.Join(err, recoverErr)
			}
			if outcome == recoveryCommitted {
				return nil
			}
		}
		return err
	}
	_ = workspace.completeCommitted()
	return nil
}

type mutationJournal struct {
	SchemaVersion            int    `json:"schema_version"`
	Operation                string `json:"operation"`
	ProblemID                int64  `json:"problem_id"`
	LiveDir                  string `json:"live_dir"`
	StagingDir               string `json:"staging_dir"`
	BackupDir                string `json:"backup_dir"`
	ExpectedAggregateVersion int64  `json:"expected_aggregate_version"`
	TargetAggregateVersion   int64  `json:"target_aggregate_version"`
	ArtifactSHA256           string `json:"artifact_sha256"`
	Phase                    string `json:"phase"`
	UpdatedAtUTC             string `json:"updated_at_utc"`
}

type workspace struct {
	root            string
	journalPath     string
	operation       string
	liveDir         string
	stagingDir      string
	backupDir       string
	problemID       int64
	expectedVersion int64
	journalWritten  bool
}

func newReplaceWorkspace(root string, problemID int64, liveDir string, expectedVersion int64) (*workspace, error) {
	root, err := cleanRoot(root)
	if err != nil {
		return nil, err
	}
	liveDir, err = containedDirectory(root, liveDir, false)
	if err != nil {
		return nil, err
	}
	base := filepath.Base(liveDir)
	return &workspace{
		root:            root,
		journalPath:     journalPath(root, problemID),
		operation:       "replace",
		liveDir:         liveDir,
		stagingDir:      filepath.Join(root, "."+base+".mutation-staging"),
		backupDir:       filepath.Join(root, "."+base+".mutation-backup"),
		problemID:       problemID,
		expectedVersion: expectedVersion,
	}, nil
}

func newCreateWorkspace(root string, problemID int64, liveDir string) (*workspace, error) {
	root, err := cleanRoot(root)
	if err != nil {
		return nil, err
	}
	liveDir, err = containedDirectory(root, liveDir, true)
	if err != nil {
		return nil, err
	}
	base := filepath.Base(liveDir)
	return &workspace{
		root:            root,
		journalPath:     journalPath(root, problemID),
		operation:       "create",
		liveDir:         liveDir,
		stagingDir:      filepath.Join(root, "."+base+".mutation-staging"),
		problemID:       problemID,
		expectedVersion: 0,
	}, nil
}

func newDeleteWorkspace(root string, problemID int64, liveDir string, expectedVersion int64) (*workspace, error) {
	root, err := cleanRoot(root)
	if err != nil {
		return nil, err
	}
	liveDir, err = containedDirectory(root, liveDir, false)
	if err != nil {
		return nil, err
	}
	base := filepath.Base(liveDir)
	return &workspace{
		root:            root,
		journalPath:     journalPath(root, problemID),
		operation:       "delete",
		liveDir:         liveDir,
		backupDir:       filepath.Join(root, "."+base+".mutation-backup"),
		problemID:       problemID,
		expectedVersion: expectedVersion,
	}, nil
}

func (w *workspace) cloneLiveTree() error {
	if _, err := os.Stat(w.journalPath); err == nil {
		return fmt.Errorf("unrecovered problem mutation journal remains: %s", w.journalPath)
	} else if !os.IsNotExist(err) {
		return err
	}
	if _, err := os.Stat(w.backupDir); err == nil {
		return fmt.Errorf("orphan problem mutation backup requires operator attention: %s", w.backupDir)
	} else if !os.IsNotExist(err) {
		return err
	}
	if err := os.RemoveAll(w.stagingDir); err != nil {
		return err
	}
	return copyTree(w.liveDir, w.stagingDir)
}

func (w *workspace) publishBeforeCommit(artifact eventing.ArtifactRef, targetVersion int64) error {
	journal := mutationJournal{
		SchemaVersion:            journalSchemaVersion,
		Operation:                w.operation,
		ProblemID:                w.problemID,
		LiveDir:                  w.liveDir,
		StagingDir:               w.stagingDir,
		BackupDir:                w.backupDir,
		ExpectedAggregateVersion: w.expectedVersion,
		TargetAggregateVersion:   targetVersion,
		ArtifactSHA256:           strings.ToLower(strings.TrimSpace(artifact.SHA256)),
		Phase:                    "prepared",
	}
	if err := writeJournal(w.journalPath, &journal); err != nil {
		return err
	}
	w.journalWritten = true
	if w.operation == "replace" {
		if err := os.Rename(w.liveDir, w.backupDir); err != nil {
			return err
		}
		journal.Phase = "backup_moved"
		if err := writeJournal(w.journalPath, &journal); err != nil {
			return err
		}
	} else if w.operation != "create" {
		return fmt.Errorf("unsupported package publish operation: %s", w.operation)
	}
	if err := os.Rename(w.stagingDir, w.liveDir); err != nil {
		return err
	}
	if err := syncDirectory(w.root); err != nil {
		return err
	}
	journal.Phase = "live_published"
	return writeJournal(w.journalPath, &journal)
}

func (w *workspace) publishDeleteBeforeCommit(artifactSHA256 string, targetVersion int64) error {
	journal := mutationJournal{
		SchemaVersion:            journalSchemaVersion,
		Operation:                "delete",
		ProblemID:                w.problemID,
		LiveDir:                  w.liveDir,
		BackupDir:                w.backupDir,
		ExpectedAggregateVersion: w.expectedVersion,
		TargetAggregateVersion:   targetVersion,
		ArtifactSHA256:           strings.ToLower(strings.TrimSpace(artifactSHA256)),
		Phase:                    "prepared",
	}
	if err := writeJournal(w.journalPath, &journal); err != nil {
		return err
	}
	w.journalWritten = true
	if err := os.Rename(w.liveDir, w.backupDir); err != nil {
		return err
	}
	if err := syncDirectory(w.root); err != nil {
		return err
	}
	journal.Phase = "live_removed"
	return writeJournal(w.journalPath, &journal)
}

func (w *workspace) abortStaging() error {
	if w == nil || w.stagingDir == "" {
		return nil
	}
	return os.RemoveAll(w.stagingDir)
}

func (w *workspace) completeCommitted() error {
	if w.backupDir != "" {
		if err := os.RemoveAll(w.backupDir); err != nil {
			return err
		}
	}
	if w.stagingDir != "" {
		if err := os.RemoveAll(w.stagingDir); err != nil {
			return err
		}
	}
	if err := syncDirectory(w.root); err != nil {
		return err
	}
	if err := os.Remove(w.journalPath); err != nil && !os.IsNotExist(err) {
		return err
	}
	return syncDirectory(filepath.Dir(w.journalPath))
}

// RecoverProblem acquires the same advisory lock used by online mutations and
// resolves a durable journal. It is suitable for startup recovery.
func RecoverProblem(ctx context.Context, repo *repository.Repository, root string, problemID int64) error {
	session, err := repo.LockProblemMutation(ctx, problemID)
	if err != nil {
		return err
	}
	defer func() { _ = session.Close() }()
	return RecoverProblemLocked(ctx, session.Repository(), root, problemID)
}

// RecoverProblemLocked must be called while holding the problem advisory lock.
func RecoverProblemLocked(ctx context.Context, repo *repository.Repository, root string, problemID int64) error {
	_, err := recoverProblemLocked(ctx, repo, root, problemID)
	return err
}

type recoveryOutcome uint8

const (
	recoveryNone recoveryOutcome = iota
	recoveryRolledBack
	recoveryCommitted
)

// recoverAfterTransactionError first uses the still-locked session. If a
// transport failure made that connection unusable, it closes the physical
// session (releasing its advisory lock), reacquires the same problem lock, and
// resumes recovery. If another process won that race and already consumed the
// journal, the append-only outbox proves the exact committed version.
func recoverAfterTransactionError(
	ctx context.Context,
	rootRepo *repository.Repository,
	session *repository.ProblemMutationSession,
	workspace *workspace,
	artifactSHA256 string,
) (recoveryOutcome, error) {
	outcome, firstErr := recoverProblemLocked(ctx, session.Repository(), workspace.root, workspace.problemID)
	if firstErr == nil {
		return outcome, nil
	}

	_ = session.Close()
	fresh, lockErr := rootRepo.LockProblemMutation(ctx, workspace.problemID)
	if lockErr != nil {
		return recoveryNone, errors.Join(firstErr, lockErr)
	}
	defer func() { _ = fresh.Close() }()
	outcome, recoverErr := recoverProblemLocked(ctx, fresh.Repository(), workspace.root, workspace.problemID)
	if recoverErr != nil {
		return recoveryNone, errors.Join(firstErr, recoverErr)
	}
	if outcome != recoveryNone {
		return outcome, nil
	}

	targetVersion := workspace.expectedVersion + 1
	switch workspace.operation {
	case "replace", "create":
		matches, err := fresh.Repository().ProblemSnapshotVersionMatches(ctx, workspace.problemID, targetVersion, artifactSHA256)
		if err != nil {
			return recoveryNone, errors.Join(firstErr, err)
		}
		if matches {
			return recoveryCommitted, nil
		}
	case "delete":
		exists, err := fresh.Repository().ProblemDeletionVersionExists(ctx, workspace.problemID, targetVersion)
		if err != nil {
			return recoveryNone, errors.Join(firstErr, err)
		}
		if exists {
			return recoveryCommitted, nil
		}
	default:
		return recoveryNone, fmt.Errorf("invalid problem mutation operation: %s", workspace.operation)
	}

	state, stateErr := fresh.Repository().ProblemProjectionState(ctx, workspace.problemID)
	if workspace.operation == "create" && errors.Is(stateErr, pgx.ErrNoRows) {
		return recoveryRolledBack, nil
	}
	if stateErr == nil && state.AggregateVersion == workspace.expectedVersion {
		return recoveryRolledBack, nil
	}
	if stateErr != nil {
		return recoveryNone, errors.Join(firstErr, stateErr)
	}
	return recoveryNone, fmt.Errorf(
		"%w: problem %d advanced after journal recovery without an exact version witness",
		ErrRecoveryNeedsAttention,
		workspace.problemID,
	)
}

// recoverProblemLocked returns the durable side of an ambiguous transaction
// outcome in addition to repairing the filesystem. Callers that already
// returned a COMMIT error use it to avoid reporting failure after a proven
// commit.
func recoverProblemLocked(ctx context.Context, repo *repository.Repository, root string, problemID int64) (recoveryOutcome, error) {
	root, err := cleanRoot(root)
	if err != nil {
		return recoveryNone, err
	}
	path := journalPath(root, problemID)
	journal, err := readJournal(path, root, problemID)
	if os.IsNotExist(err) {
		return recoveryNone, nil
	}
	if err != nil {
		return recoveryNone, err
	}
	state, stateErr := repo.ProblemProjectionState(ctx, problemID)
	if stateErr != nil && !errors.Is(stateErr, pgx.ErrNoRows) {
		return recoveryNone, stateErr
	}

	committed := false
	rolledBack := false
	switch journal.Operation {
	case "replace", "create":
		committed = stateErr == nil &&
			state.AggregateVersion == journal.TargetAggregateVersion &&
			strings.EqualFold(state.PackageArtifactSHA256, journal.ArtifactSHA256)
		rolledBack = (journal.Operation == "create" && errors.Is(stateErr, pgx.ErrNoRows)) ||
			(stateErr == nil && state.AggregateVersion == journal.ExpectedAggregateVersion)
	case "delete":
		committed = errors.Is(stateErr, pgx.ErrNoRows)
		rolledBack = stateErr == nil && state.AggregateVersion == journal.ExpectedAggregateVersion
	default:
		return recoveryNone, fmt.Errorf("invalid problem mutation journal operation: %s", journal.Operation)
	}
	if !committed && !rolledBack {
		return recoveryNone, fmt.Errorf(
			"%w: problem %d DB version/artifact does not match journal transition %d -> %d",
			ErrRecoveryNeedsAttention,
			problemID,
			journal.ExpectedAggregateVersion,
			journal.TargetAggregateVersion,
		)
	}

	if err := recoverFilesystem(journal, committed); err != nil {
		return recoveryNone, err
	}
	if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
		return recoveryNone, err
	}
	if err := syncDirectory(root); err != nil {
		return recoveryNone, err
	}
	if err := syncDirectory(filepath.Dir(path)); err != nil {
		return recoveryNone, err
	}
	if committed {
		return recoveryCommitted, nil
	}
	return recoveryRolledBack, nil
}

func recoverFilesystem(journal *mutationJournal, committed bool) error {
	if committed {
		if journal.Operation == "delete" {
			if journal.Phase != "live_removed" {
				return fmt.Errorf("%w: DB deleted but package removal phase is %s", ErrRecoveryNeedsAttention, journal.Phase)
			}
			return os.RemoveAll(journal.BackupDir)
		}
		if journal.Phase != "live_published" {
			return fmt.Errorf("%w: DB committed but package publish phase is %s", ErrRecoveryNeedsAttention, journal.Phase)
		}
		actualDigest, err := packageDigest(journal.LiveDir)
		if err != nil || !strings.EqualFold(actualDigest, journal.ArtifactSHA256) {
			return fmt.Errorf("%w: committed live package does not match journal artifact", ErrRecoveryNeedsAttention)
		}
		if journal.BackupDir != "" {
			if err := os.RemoveAll(journal.BackupDir); err != nil {
				return err
			}
		}
		if journal.StagingDir != "" {
			_ = os.RemoveAll(journal.StagingDir)
		}
		return nil
	}

	// DB is still at the version observed before publication. Restore the
	// original directory byte-for-byte from the rename backup.
	if journal.Operation == "create" {
		_, stagingErr := os.Stat(journal.StagingDir)
		_, liveErr := os.Stat(journal.LiveDir)
		if stagingErr == nil {
			// The staged directory still existing proves the create rename did
			// not succeed. A colliding live directory is therefore not ours.
			return os.RemoveAll(journal.StagingDir)
		}
		if !os.IsNotExist(stagingErr) {
			return stagingErr
		}
		if liveErr == nil {
			actualDigest, err := packageDigest(journal.LiveDir)
			if err != nil || !strings.EqualFold(actualDigest, journal.ArtifactSHA256) {
				return fmt.Errorf("%w: rolled-back create live tree cannot be attributed to the journal", ErrRecoveryNeedsAttention)
			}
			return os.RemoveAll(journal.LiveDir)
		}
		if !os.IsNotExist(liveErr) {
			return liveErr
		}
		return nil
	}
	if _, err := os.Stat(journal.BackupDir); err == nil {
		if _, liveErr := os.Stat(journal.LiveDir); liveErr == nil {
			if err := os.RemoveAll(journal.LiveDir); err != nil {
				return err
			}
		} else if !os.IsNotExist(liveErr) {
			return liveErr
		}
		if err := os.Rename(journal.BackupDir, journal.LiveDir); err != nil {
			return err
		}
	} else if !os.IsNotExist(err) {
		return err
	} else {
		if journal.Phase != "prepared" {
			return fmt.Errorf("%w: rollback backup is missing in phase %s", ErrRecoveryNeedsAttention, journal.Phase)
		}
		if _, liveErr := os.Stat(journal.LiveDir); liveErr != nil {
			return fmt.Errorf("%w: rollback live tree is missing without a backup: %v", ErrRecoveryNeedsAttention, liveErr)
		}
	}
	if journal.StagingDir != "" {
		_ = os.RemoveAll(journal.StagingDir)
	}
	return nil
}

// RecoverAll scans only well-formed journal names and fails service startup if
// any transition cannot be proved committed or rolled back.
func RecoverAll(ctx context.Context, repo *repository.Repository, root string) error {
	root, err := cleanRoot(root)
	if err != nil {
		return err
	}
	dir := filepath.Join(root, ".ojos-mutations")
	entries, err := os.ReadDir(dir)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasPrefix(entry.Name(), "problem-") || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}
		var problemID int64
		if _, err := fmt.Sscanf(entry.Name(), "problem-%d.json", &problemID); err != nil || problemID <= 0 || entry.Name() != fmt.Sprintf("problem-%d.json", problemID) {
			return fmt.Errorf("invalid problem mutation journal name: %s", entry.Name())
		}
		if err := RecoverProblem(ctx, repo, root, problemID); err != nil {
			return err
		}
	}
	return nil
}

func rebaseLocalFiles(files []packagefs.IndexedFile, stagingDir string, liveDir string) ([]packagefs.IndexedFile, error) {
	stagingDir, err := filepath.Abs(stagingDir)
	if err != nil {
		return nil, err
	}
	liveDir, err = filepath.Abs(liveDir)
	if err != nil {
		return nil, err
	}
	result := append([]packagefs.IndexedFile(nil), files...)
	for index := range result {
		if strings.HasPrefix(result[index].StoragePath, "storage://") {
			continue
		}
		path, err := filepath.Abs(result[index].StoragePath)
		if err != nil {
			return nil, err
		}
		rel, err := filepath.Rel(stagingDir, path)
		if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) || filepath.IsAbs(rel) {
			return nil, fmt.Errorf("indexed package file is outside staging tree: %s", result[index].StoragePath)
		}
		result[index].StoragePath = filepath.Join(liveDir, rel)
	}
	return result, nil
}

func packageDigest(dir string) (string, error) {
	zipPath, digest, _, err := problemstorage.BuildDeterministicPackageArtifact(dir)
	if zipPath != "" {
		_ = os.Remove(zipPath)
	}
	return digest, err
}

func validateStagedPackage(dir string) error {
	validation, err := packagefs.ValidatePackage(dir)
	if err != nil {
		return fmt.Errorf("inspect staged problem package: %w", err)
	}
	if validation.Valid {
		return nil
	}
	if len(validation.Errors) == 0 {
		return errors.New("staged problem package is invalid")
	}
	issue := validation.Errors[0]
	location := issue.Path
	if issue.CaseNo > 0 {
		location = fmt.Sprintf("%s case %d", location, issue.CaseNo)
	}
	return fmt.Errorf(
		"staged problem package is invalid: %s at %s: %s",
		issue.Code,
		location,
		issue.Message,
	)
}

func cleanRoot(root string) (string, error) {
	if strings.TrimSpace(root) == "" {
		return "", errors.New("empty problems root")
	}
	root, err := filepath.Abs(root)
	if err != nil {
		return "", err
	}
	if err := os.MkdirAll(root, 0o755); err != nil {
		return "", err
	}
	return filepath.Clean(root), nil
}

func containedDirectory(root string, path string, allowMissing bool) (string, error) {
	if strings.TrimSpace(path) == "" {
		return "", errors.New("empty package directory")
	}
	path, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	rel, err := filepath.Rel(root, path)
	if err != nil || rel == "." || rel == "" || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) || filepath.IsAbs(rel) {
		return "", fmt.Errorf("package directory is outside problems root: %s", path)
	}
	if !allowMissing {
		info, err := os.Stat(path)
		if err != nil {
			return "", err
		}
		if !info.IsDir() {
			return "", fmt.Errorf("package path is not a directory: %s", path)
		}
	}
	return filepath.Clean(path), nil
}

func journalPath(root string, problemID int64) string {
	return filepath.Join(root, ".ojos-mutations", fmt.Sprintf("problem-%d.json", problemID))
}

func writeJournal(path string, journal *mutationJournal) error {
	journal.UpdatedAtUTC = time.Now().UTC().Format(time.RFC3339Nano)
	data, err := json.Marshal(journal)
	if err != nil {
		return err
	}
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(dir, ".problem-mutation-*.tmp")
	if err != nil {
		return err
	}
	tmpPath := tmp.Name()
	cleanup := func() { _ = os.Remove(tmpPath) }
	if err := tmp.Chmod(0o600); err != nil {
		_ = tmp.Close()
		cleanup()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		_ = tmp.Close()
		cleanup()
		return err
	}
	if err := tmp.Sync(); err != nil {
		_ = tmp.Close()
		cleanup()
		return err
	}
	if err := tmp.Close(); err != nil {
		cleanup()
		return err
	}
	if err := os.Rename(tmpPath, path); err != nil {
		cleanup()
		return err
	}
	return syncDirectory(dir)
}

func readJournal(path string, root string, problemID int64) (*mutationJournal, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var journal mutationJournal
	if err := json.Unmarshal(data, &journal); err != nil {
		return nil, fmt.Errorf("decode problem mutation journal %s: %w", path, err)
	}
	if journal.SchemaVersion != journalSchemaVersion || journal.ProblemID != problemID ||
		(journal.Operation != "replace" && journal.Operation != "create" && journal.Operation != "delete") {
		return nil, fmt.Errorf("invalid problem mutation journal identity: %s", path)
	}
	paths := map[string]string{"live": journal.LiveDir}
	if journal.StagingDir != "" {
		paths["staging"] = journal.StagingDir
	}
	if journal.BackupDir != "" {
		paths["backup"] = journal.BackupDir
	}
	for label, value := range paths {
		clean, err := containedDirectory(root, value, true)
		if err != nil || clean != filepath.Clean(value) {
			return nil, fmt.Errorf("invalid %s directory in mutation journal: %s", label, value)
		}
	}
	if journal.TargetAggregateVersion != journal.ExpectedAggregateVersion+1 ||
		(journal.Operation != "delete" && strings.TrimSpace(journal.ArtifactSHA256) == "") {
		return nil, errors.New("invalid problem mutation journal version transition")
	}
	validPhase := journal.Phase == "prepared" ||
		(journal.Operation == "replace" && journal.Phase == "backup_moved") ||
		((journal.Operation == "replace" || journal.Operation == "create") && journal.Phase == "live_published") ||
		(journal.Operation == "delete" && journal.Phase == "live_removed")
	if !validPhase {
		return nil, fmt.Errorf("invalid problem mutation journal phase: %s", journal.Phase)
	}
	return &journal, nil
}

func copyTree(source string, destination string) error {
	if err := os.Mkdir(destination, 0o755); err != nil {
		return err
	}
	return filepath.WalkDir(source, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		rel, err := filepath.Rel(source, path)
		if err != nil {
			return err
		}
		if rel == "." {
			return nil
		}
		target := filepath.Join(destination, rel)
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if info.Mode()&os.ModeSymlink != 0 {
			return fmt.Errorf("package staging refuses symlink: %s", path)
		}
		if entry.IsDir() {
			return os.Mkdir(target, info.Mode().Perm())
		}
		if !info.Mode().IsRegular() {
			return fmt.Errorf("package staging refuses non-regular file: %s", path)
		}
		in, err := os.Open(path)
		if err != nil {
			return err
		}
		out, err := os.OpenFile(target, os.O_CREATE|os.O_EXCL|os.O_WRONLY, info.Mode().Perm())
		if err != nil {
			_ = in.Close()
			return err
		}
		_, copyErr := io.Copy(out, in)
		syncErr := out.Sync()
		closeOutErr := out.Close()
		closeInErr := in.Close()
		return errors.Join(copyErr, syncErr, closeOutErr, closeInErr)
	})
}

func syncTree(root string) error {
	var directories []string
	err := filepath.WalkDir(root, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if entry.IsDir() {
			directories = append(directories, path)
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() {
			return fmt.Errorf("package staging refuses non-regular entry: %s", path)
		}
		var file *os.File
		if runtime.GOOS == "windows" {
			// FlushFileBuffers requires a writable handle on Windows. Opening
			// read-only works on Unix and also preserves read-only package files.
			file, err = os.OpenFile(path, os.O_WRONLY, 0)
		} else {
			file, err = os.Open(path)
		}
		if err != nil {
			return err
		}
		syncErr := file.Sync()
		closeErr := file.Close()
		return errors.Join(syncErr, closeErr)
	})
	if err != nil {
		return err
	}
	for index := len(directories) - 1; index >= 0; index-- {
		if err := syncDirectory(directories[index]); err != nil {
			return err
		}
	}
	return nil
}

func syncDirectory(path string) error {
	if runtime.GOOS == "windows" {
		return nil
	}
	dir, err := os.Open(path)
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}
