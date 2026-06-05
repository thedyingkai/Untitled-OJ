package repository

import (
	"context"
	"fmt"
	"time"

	"ojos-problem-api/internal/packagefs"

	"github.com/jackc/pgx/v5/pgxpool"
)

type Repository struct {
	db *pgxpool.Pool
}

func New(db *pgxpool.Pool) *Repository {
	return &Repository{db: db}
}

type Problem struct {
	ID             int64
	Slug           string
	Title          string
	Statement      string
	ProblemType    string
	Visibility     string
	PackageDir     string
	ManifestPath   string
	ManifestSha256 string
	SourceFormat   string
	Status         string
	TimeLimitMs    int
	MemoryLimitMb  int
	CreatedBy      int64
	CreatedAt      time.Time
	UpdatedAt      time.Time
}

type CreateProblemArg struct {
	Title         string
	Statement     string
	ProblemType   string
	Visibility    string
	TimeLimitMs   int
	MemoryLimitMb int
	CreatedBy     int64
}

func (r *Repository) InsertProblem(ctx context.Context, arg CreateProblemArg) (int64, error) {
	var id int64

	err := r.db.QueryRow(
		ctx,
		`
INSERT INTO problems(
    title,
    statement,
    problem_type,
    visibility,
    time_limit_ms,
    memory_limit_mb,
    status,
    source_format,
    created_by,
    created_at,
    updated_at
)
VALUES($1, $2, $3, $4, $5, $6, 'draft', 'ojos', $7, NOW(), NOW())
RETURNING id
`,
		arg.Title,
		arg.Statement,
		arg.ProblemType,
		arg.Visibility,
		arg.TimeLimitMs,
		arg.MemoryLimitMb,
		arg.CreatedBy,
	).Scan(&id)

	return id, err
}

func (r *Repository) UpdateProblemPackage(
	ctx context.Context,
	id int64,
	slug string,
	packageDir string,
	manifestPath string,
	manifestSha string,
) error {
	_, err := r.db.Exec(
		ctx,
		`
UPDATE problems
SET
    slug = $2,
    package_dir = $3,
    manifest_path = $4,
    manifest_sha256 = $5,
    updated_at = NOW()
WHERE id = $1
`,
		id,
		slug,
		packageDir,
		manifestPath,
		manifestSha,
	)
	return err
}

func (r *Repository) UpdateProblem(
	ctx context.Context,
	id int64,
	title string,
	statement string,
	problemType string,
	visibility string,
	status string,
	timeLimitMs int,
	memoryLimitMb int,
	manifestSha string,
) error {
	_, err := r.db.Exec(
		ctx,
		`
UPDATE problems
SET
    title = COALESCE(NULLIF($2, ''), title),
    statement = COALESCE(NULLIF($3, ''), statement),
    problem_type = COALESCE(NULLIF($4, ''), problem_type),
    visibility = COALESCE(NULLIF($5, ''), visibility),
    status = COALESCE(NULLIF($6, ''), status),
    time_limit_ms = CASE WHEN $7 > 0 THEN $7 ELSE time_limit_ms END,
    memory_limit_mb = CASE WHEN $8 > 0 THEN $8 ELSE memory_limit_mb END,
    manifest_sha256 = COALESCE(NULLIF($9, ''), manifest_sha256),
    updated_at = NOW()
WHERE id = $1
`,
		id,
		title,
		statement,
		problemType,
		visibility,
		status,
		timeLimitMs,
		memoryLimitMb,
		manifestSha,
	)
	return err
}

func (r *Repository) GetProblem(ctx context.Context, id int64) (*Problem, error) {
	var p Problem

	err := r.db.QueryRow(
		ctx,
		`
SELECT
    id,
    COALESCE(slug, ''),
    title,
    COALESCE(statement, ''),
    COALESCE(problem_type, 'traditional'),
    COALESCE(visibility, 'private'),
    COALESCE(package_dir, ''),
    COALESCE(manifest_path, ''),
    COALESCE(manifest_sha256, ''),
    COALESCE(source_format, 'ojos'),
    COALESCE(status, 'draft'),
    time_limit_ms,
    memory_limit_mb,
    COALESCE(created_by, 0),
    created_at,
    updated_at
FROM problems
WHERE id = $1
`,
		id,
	).Scan(
		&p.ID,
		&p.Slug,
		&p.Title,
		&p.Statement,
		&p.ProblemType,
		&p.Visibility,
		&p.PackageDir,
		&p.ManifestPath,
		&p.ManifestSha256,
		&p.SourceFormat,
		&p.Status,
		&p.TimeLimitMs,
		&p.MemoryLimitMb,
		&p.CreatedBy,
		&p.CreatedAt,
		&p.UpdatedAt,
	)

	if err != nil {
		return nil, err
	}

	return &p, nil
}

func (r *Repository) ListProblems(ctx context.Context, page int, pageSize int) ([]Problem, int64, error) {
	if page <= 0 {
		page = 1
	}
	if pageSize <= 0 {
		pageSize = 20
	}
	if pageSize > 100 {
		pageSize = 100
	}

	offset := (page - 1) * pageSize

	var total int64
	if err := r.db.QueryRow(ctx, `SELECT COUNT(*) FROM problems`).Scan(&total); err != nil {
		return nil, 0, err
	}

	rows, err := r.db.Query(
		ctx,
		`
SELECT
    id,
    COALESCE(slug, ''),
    title,
    COALESCE(statement, ''),
    COALESCE(problem_type, 'traditional'),
    COALESCE(visibility, 'private'),
    COALESCE(package_dir, ''),
    COALESCE(manifest_path, ''),
    COALESCE(manifest_sha256, ''),
    COALESCE(source_format, 'ojos'),
    COALESCE(status, 'draft'),
    time_limit_ms,
    memory_limit_mb,
    COALESCE(created_by, 0),
    created_at,
    updated_at
FROM problems
ORDER BY id DESC
LIMIT $1 OFFSET $2
`,
		pageSize,
		offset,
	)
	if err != nil {
		return nil, 0, err
	}
	defer rows.Close()

	var problems []Problem
	for rows.Next() {
		var p Problem
		if err := rows.Scan(
			&p.ID,
			&p.Slug,
			&p.Title,
			&p.Statement,
			&p.ProblemType,
			&p.Visibility,
			&p.PackageDir,
			&p.ManifestPath,
			&p.ManifestSha256,
			&p.SourceFormat,
			&p.Status,
			&p.TimeLimitMs,
			&p.MemoryLimitMb,
			&p.CreatedBy,
			&p.CreatedAt,
			&p.UpdatedAt,
		); err != nil {
			return nil, 0, err
		}
		problems = append(problems, p)
	}

	if err := rows.Err(); err != nil {
		return nil, 0, err
	}

	return problems, total, nil
}

func (r *Repository) BindProblemOwner(ctx context.Context, userID int64, problemID int64) error {
	_, err := r.db.Exec(
		ctx,
		`
INSERT INTO role_bindings(
    principal_type,
    principal_id,
    role_id,
    scope_type,
    scope_id,
    granted_by_type,
    granted_by_id,
    created_at
)
SELECT
    'user',
    $1,
    roles.id,
    'problem',
    $2,
    'user',
    $1,
    NOW()
FROM roles
WHERE roles.name = 'problem_owner'
  AND NOT EXISTS (
      SELECT 1
      FROM role_bindings rb
      WHERE rb.principal_type = 'user'
        AND rb.principal_id = $1
        AND rb.role_id = roles.id
        AND rb.scope_type = 'problem'
        AND rb.scope_id = $2
  )
`,
		userID,
		problemID,
	)
	return err
}

func (r *Repository) UpsertProblemFiles(ctx context.Context, problemID int64, files []packagefs.IndexedFile) error {
	for _, f := range files {
		_, err := r.db.Exec(
			ctx,
			`
INSERT INTO problem_files(
    problem_id,
    logical_path,
    file_kind,
    storage_path,
    sha256,
    size_bytes,
    mime_type,
    created_at
)
VALUES($1, $2, $3, $4, $5, $6, $7, NOW())
ON CONFLICT(problem_id, logical_path)
DO UPDATE SET
    file_kind = EXCLUDED.file_kind,
    storage_path = EXCLUDED.storage_path,
    sha256 = EXCLUDED.sha256,
    size_bytes = EXCLUDED.size_bytes,
    mime_type = EXCLUDED.mime_type
`,
			problemID,
			f.LogicalPath,
			f.FileKind,
			f.StoragePath,
			f.Sha256,
			f.SizeBytes,
			f.MimeType,
		)
		if err != nil {
			return err
		}
	}

	return nil
}

func (r *Repository) DeleteProblemFiles(ctx context.Context, problemID int64, logicalPaths []string) error {
	for _, logical := range logicalPaths {
		_, err := r.db.Exec(
			ctx,
			`
DELETE FROM problem_files
WHERE problem_id = $1 AND logical_path = $2
`,
			problemID,
			logical,
		)
		if err != nil {
			return err
		}
	}
	return nil
}

func (r *Repository) DeleteProblem(ctx context.Context, problemID int64) error {
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return err
	}
	defer func() {
		_ = tx.Rollback(ctx)
	}()

	var submissionCount int64
	if err := tx.QueryRow(
		ctx,
		`SELECT COUNT(*) FROM submissions WHERE problem_id = $1`,
		problemID,
	).Scan(&submissionCount); err != nil {
		return err
	}

	if submissionCount > 0 {
		return fmt.Errorf("problem has submissions, refuse to delete: %d", submissionCount)
	}

	if _, err := tx.Exec(ctx, `DELETE FROM problem_files WHERE problem_id = $1`, problemID); err != nil {
		return err
	}

	if _, err := tx.Exec(
		ctx,
		`
DELETE FROM role_bindings
WHERE scope_type = 'problem'
  AND scope_id = $1
`,
		problemID,
	); err != nil {
		return err
	}

	if _, err := tx.Exec(
		ctx,
		`
DELETE FROM permission_assignments
WHERE scope_type = 'problem'
  AND scope_id = $1
`,
		problemID,
	); err != nil {
		return err
	}

	if _, err := tx.Exec(
		ctx,
		`
DELETE FROM resource_edges
WHERE (parent_type = 'problem' AND parent_id = $1)
   OR (child_type = 'problem' AND child_id = $1)
`,
		problemID,
	); err != nil {
		return err
	}

	tag, err := tx.Exec(ctx, `DELETE FROM problems WHERE id = $1`, problemID)
	if err != nil {
		return err
	}

	if tag.RowsAffected() == 0 {
		return fmt.Errorf("problem not found: %d", problemID)
	}

	return tx.Commit(ctx)
}

func (r *Repository) IsProblemOwner(ctx context.Context, userID int64, problemID int64) (bool, error) {
	var ok bool

	err := r.db.QueryRow(
		ctx,
		`
SELECT EXISTS (
    SELECT 1
    FROM role_bindings rb
    JOIN roles r ON r.id = rb.role_id
    WHERE rb.principal_type = 'user'
      AND rb.principal_id = $1
      AND rb.scope_type = 'problem'
      AND rb.scope_id = $2
      AND r.name = 'problem_owner'
)
`,
		userID,
		problemID,
	).Scan(&ok)

	return ok, err
}
