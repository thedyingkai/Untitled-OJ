package repository

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

var (
	ErrSubmissionNotFound        = errors.New("submission not found")
	ErrProblemProjectionNotReady = errors.New("problem package projection is not ready")
)

type Repository struct {
	db                    *pgxpool.Pool
	allowLegacyPackageDir bool
}

type Option func(*Repository)

// WithLegacyProblemPackageDir is a development-only compatibility option.
// Production startup rejects enabling it.
func WithLegacyProblemPackageDir(enabled bool) Option {
	return func(repository *Repository) {
		repository.allowLegacyPackageDir = enabled
	}
}

func New(db *pgxpool.Pool, options ...Option) *Repository {
	repository := &Repository{db: db}
	for _, option := range options {
		if option != nil {
			option(repository)
		}
	}
	return repository
}

type ProblemMeta struct {
	ID                       int64
	PackageDir               string
	Status                   string
	Visibility               string
	CreatedBy                int64
	AggregateVersion         int64
	PackageRevision          int64
	PackageArtifactURI       string
	PackageArtifactSHA256    string
	PackageArtifactSizeBytes int64
	Deleted                  bool
}

func (p ProblemMeta) HasManagedPackageArtifact() bool {
	return p.AggregateVersion > 0 &&
		p.PackageRevision > 0 &&
		strings.TrimSpace(p.PackageArtifactURI) != "" &&
		isLowerHexDigest(p.PackageArtifactSHA256) &&
		p.PackageArtifactSizeBytes > 0
}

func (p ProblemMeta) HasAnyProjectionArtifactState() bool {
	return p.AggregateVersion != 0 ||
		p.PackageRevision != 0 ||
		strings.TrimSpace(p.PackageArtifactURI) != "" ||
		strings.TrimSpace(p.PackageArtifactSHA256) != "" ||
		p.PackageArtifactSizeBytes != 0
}

func (r *Repository) GetProblemMeta(ctx context.Context, id int64) (*ProblemMeta, error) {
	var p ProblemMeta

	err := r.db.QueryRow(
		ctx,
		`
SELECT
    id,
    package_dir,
    status,
    visibility,
    COALESCE(created_by, 0),
    aggregate_version,
    package_revision,
    package_artifact_uri,
    package_artifact_sha256,
    package_artifact_size_bytes,
    deleted
FROM problems
WHERE id = $1
`,
		id,
	).Scan(&p.ID, &p.PackageDir, &p.Status, &p.Visibility, &p.CreatedBy, &p.AggregateVersion, &p.PackageRevision, &p.PackageArtifactURI, &p.PackageArtifactSHA256, &p.PackageArtifactSizeBytes, &p.Deleted)

	if err != nil {
		return nil, err
	}

	return &p, nil
}

func (r *Repository) CreateSubmission(
	ctx context.Context,
	problemID int64,
	userID int64,
	language string,
) (int64, error) {
	var id int64

	err := r.db.QueryRow(
		ctx,
		`
INSERT INTO submissions(
    problem_id,
    user_id,
    language,
    status,
    score,
    time_ms,
    memory_kb,
    message,
    created_at,
    updated_at,
    problem_aggregate_version,
    problem_package_revision,
    problem_artifact_uri,
    problem_artifact_sha256,
    problem_artifact_size_bytes
)
SELECT
    p.id, $2, $3, 'PENDING', 0, 0, 0, '', NOW(), NOW(),
    p.aggregate_version,
    p.package_revision,
    p.package_artifact_uri,
    p.package_artifact_sha256,
    p.package_artifact_size_bytes
FROM problems p
WHERE p.id = $1
  AND p.deleted = FALSE
  AND (
      (
          p.aggregate_version > 0
          AND p.package_revision > 0
          AND p.package_artifact_uri <> ''
          AND p.package_artifact_sha256 ~ '^[a-f0-9]{64}$'
          AND p.package_artifact_size_bytes > 0
      )
      OR (
          $4::boolean
          AND p.aggregate_version = 0
          AND p.package_revision = 0
          AND p.package_artifact_uri = ''
          AND p.package_artifact_sha256 = ''
          AND p.package_artifact_size_bytes = 0
          AND p.package_dir <> ''
      )
  )
RETURNING id
`,
		problemID,
		userID,
		language,
		r.allowLegacyPackageDir,
	).Scan(&id)
	if errors.Is(err, pgx.ErrNoRows) {
		return 0, ErrProblemProjectionNotReady
	}

	return id, err
}

func isLowerHexDigest(value string) bool {
	value = strings.TrimSpace(value)
	if len(value) != 64 {
		return false
	}
	for _, char := range value {
		if (char < '0' || char > '9') && (char < 'a' || char > 'f') {
			return false
		}
	}
	return true
}

func (r *Repository) UpdateSubmissionSource(
	ctx context.Context,
	submissionID int64,
	codePath string,
	codeSha256 string,
	resultPath string,
) error {
	_, err := r.db.Exec(
		ctx,
		`
UPDATE submissions
SET
    code_path = $2,
    code_sha256 = $3,
    result_path = $4,
    updated_at = NOW()
WHERE id = $1
`,
		submissionID,
		codePath,
		codeSha256,
		resultPath,
	)
	return err
}

func (r *Repository) MarkSubmissionSystemError(ctx context.Context, submissionID int64, message string) error {
	_, err := r.db.Exec(
		ctx,
		`
UPDATE submissions
SET
    status = 'SYSTEM_ERROR',
    message = $2,
    updated_at = NOW(),
    judged_at = NOW()
WHERE id = $1
`,
		submissionID,
		message,
	)
	return err
}

type SubmissionView struct {
	ID                       int64
	ProblemID                int64
	UserID                   int64
	Language                 string
	Status                   string
	Score                    int
	TimeMS                   int
	MemoryKB                 int
	Message                  string
	CodePath                 string
	CodeSha256               string
	ResultPath               string
	CreatedAt                time.Time
	UpdatedAt                time.Time
	JudgedAt                 *time.Time
	CancelledAt              *time.Time
	CancelReason             string
	ProblemAggregateVersion  int64
	ProblemPackageRevision   int64
	ProblemArtifactURI       string
	ProblemArtifactSHA256    string
	ProblemArtifactSizeBytes int64
}

func (r *Repository) GetSubmission(ctx context.Context, id int64) (*SubmissionView, error) {
	var s SubmissionView
	var message *string
	var cancelReason *string

	err := r.db.QueryRow(
		ctx,
		`
SELECT
    id,
    problem_id,
    user_id,
    language,
    status,
    score,
    time_ms,
    memory_kb,
    message,
    code_path,
    code_sha256,
    result_path,
    created_at,
    updated_at,
    judged_at,
    cancelled_at,
    cancel_reason,
    problem_aggregate_version,
    problem_package_revision,
    problem_artifact_uri,
    problem_artifact_sha256,
    problem_artifact_size_bytes
FROM submissions
WHERE id = $1
`,
		id,
	).Scan(
		&s.ID,
		&s.ProblemID,
		&s.UserID,
		&s.Language,
		&s.Status,
		&s.Score,
		&s.TimeMS,
		&s.MemoryKB,
		&message,
		&s.CodePath,
		&s.CodeSha256,
		&s.ResultPath,
		&s.CreatedAt,
		&s.UpdatedAt,
		&s.JudgedAt,
		&s.CancelledAt,
		&cancelReason,
		&s.ProblemAggregateVersion,
		&s.ProblemPackageRevision,
		&s.ProblemArtifactURI,
		&s.ProblemArtifactSHA256,
		&s.ProblemArtifactSizeBytes,
	)

	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return nil, ErrSubmissionNotFound
		}
		return nil, err
	}

	if message != nil {
		s.Message = *message
	}
	if cancelReason != nil {
		s.CancelReason = *cancelReason
	}

	return &s, nil
}

type ListSubmissionsFilter struct {
	Page             int
	PageSize         int
	Status           string
	ProblemID        int64
	UserID           int64
	Language         string
	CreatedFrom      *time.Time
	CreatedTo        *time.Time
	RestrictToUserID int64
}

func (r *Repository) ListSubmissions(ctx context.Context, filter ListSubmissionsFilter) ([]SubmissionView, int64, error) {
	page := filter.Page
	pageSize := filter.PageSize
	if page <= 0 {
		page = 1
	}
	if pageSize <= 0 {
		pageSize = 20
	}
	if pageSize > 100 {
		pageSize = 100
	}

	where, args := buildSubmissionListWhere(filter)

	var total int64
	if err := r.db.QueryRow(ctx, `SELECT COUNT(*) FROM submissions `+where, args...).Scan(&total); err != nil {
		return nil, 0, err
	}

	queryArgs := append([]any{}, args...)
	limitIndex := len(queryArgs) + 1
	offsetIndex := len(queryArgs) + 2
	queryArgs = append(queryArgs, pageSize, (page-1)*pageSize)

	rows, err := r.db.Query(
		ctx,
		fmt.Sprintf(`
SELECT
    id,
    problem_id,
    user_id,
    language,
    status,
    score,
    time_ms,
    memory_kb,
    message,
    code_path,
    code_sha256,
    result_path,
    created_at,
    updated_at,
    judged_at,
    cancelled_at,
    cancel_reason
FROM submissions
%s
ORDER BY id DESC
LIMIT $%d OFFSET $%d
`, where, limitIndex, offsetIndex),
		queryArgs...,
	)
	if err != nil {
		return nil, 0, err
	}
	defer rows.Close()

	submissions := make([]SubmissionView, 0)
	for rows.Next() {
		var s SubmissionView
		var message *string
		var cancelReason *string
		if err := rows.Scan(
			&s.ID,
			&s.ProblemID,
			&s.UserID,
			&s.Language,
			&s.Status,
			&s.Score,
			&s.TimeMS,
			&s.MemoryKB,
			&message,
			&s.CodePath,
			&s.CodeSha256,
			&s.ResultPath,
			&s.CreatedAt,
			&s.UpdatedAt,
			&s.JudgedAt,
			&s.CancelledAt,
			&cancelReason,
		); err != nil {
			return nil, 0, err
		}
		if message != nil {
			s.Message = *message
		}
		if cancelReason != nil {
			s.CancelReason = *cancelReason
		}
		submissions = append(submissions, s)
	}
	if err := rows.Err(); err != nil {
		return nil, 0, err
	}

	return submissions, total, nil
}

func buildSubmissionListWhere(filter ListSubmissionsFilter) (string, []any) {
	args := make([]any, 0)
	clauses := []string{"TRUE"}

	if filter.RestrictToUserID > 0 {
		args = append(args, filter.RestrictToUserID)
		clauses = append(clauses, fmt.Sprintf("user_id = $%d", len(args)))
	} else if filter.UserID > 0 {
		args = append(args, filter.UserID)
		clauses = append(clauses, fmt.Sprintf("user_id = $%d", len(args)))
	}

	if filter.ProblemID > 0 {
		args = append(args, filter.ProblemID)
		clauses = append(clauses, fmt.Sprintf("problem_id = $%d", len(args)))
	}

	if status := strings.TrimSpace(filter.Status); status != "" {
		args = append(args, status)
		clauses = append(clauses, fmt.Sprintf("status = $%d", len(args)))
	}

	if language := strings.TrimSpace(filter.Language); language != "" {
		args = append(args, language)
		clauses = append(clauses, fmt.Sprintf("language = $%d", len(args)))
	}

	if filter.CreatedFrom != nil {
		args = append(args, *filter.CreatedFrom)
		clauses = append(clauses, fmt.Sprintf("created_at >= $%d", len(args)))
	}

	if filter.CreatedTo != nil {
		args = append(args, *filter.CreatedTo)
		clauses = append(clauses, fmt.Sprintf("created_at <= $%d", len(args)))
	}

	return "WHERE " + strings.Join(clauses, " AND "), args
}

func (r *Repository) CancelSubmission(ctx context.Context, submissionID int64, userID int64, reason string) error {
	_, err := r.db.Exec(
		ctx,
		`
UPDATE submissions
SET
    status = 'CANCELLED',
    score = 0,
    message = '',
    cancelled_at = NOW(),
    cancelled_by = $2,
    cancel_reason = $3,
    updated_at = NOW()
WHERE id = $1
`,
		submissionID,
		userID,
		reason,
	)
	return err
}

func (r *Repository) ResetSubmissionsForProblem(ctx context.Context, problemID int64) ([]int64, error) {
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return nil, err
	}
	defer func() {
		_ = tx.Rollback(ctx)
	}()

	rows, err := tx.Query(
		ctx,
		`
SELECT id
FROM submissions
WHERE problem_id = $1
ORDER BY id
`,
		problemID,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var ids []int64
	for rows.Next() {
		var id int64
		if err := rows.Scan(&id); err != nil {
			return nil, err
		}
		ids = append(ids, id)
	}

	if err := rows.Err(); err != nil {
		return nil, err
	}

	_, err = tx.Exec(
		ctx,
		`
UPDATE submissions
SET
    status = 'PENDING',
    score = 0,
    time_ms = 0,
    memory_kb = 0,
    message = '',
    judged_at = NULL,
    cancelled_at = NULL,
    cancelled_by = NULL,
    cancel_reason = '',
    updated_at = NOW()
WHERE problem_id = $1
`,
		problemID,
	)
	if err != nil {
		return nil, err
	}

	if err := tx.Commit(ctx); err != nil {
		return nil, err
	}

	return ids, nil
}
