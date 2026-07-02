package repository

import (
	"context"
	"fmt"
	"strings"
	"time"

	"ojos-problem-service/internal/packagefs"

	"github.com/jackc/pgx/v5/pgxpool"
)

type Repository struct {
	db *pgxpool.Pool
}

func New(db *pgxpool.Pool) *Repository {
	return &Repository{db: db}
}

type Problem struct {
	ID              int64
	ProblemNo       string
	Slug            string
	Title           string
	Statement       string
	StatementFormat string
	Solution        string
	SolutionFormat  string
	ProblemType     string
	Visibility      string
	PackageDir      string
	ManifestPath    string
	ManifestSha256  string
	SourceFormat    string
	Status          string
	Difficulty      string
	Tags            []string
	TimeLimitMs     int
	MemoryLimitMb   int
	LanguageLimits  []ProblemLanguageLimit
	CreatedBy       int64
	CreatedAt       time.Time
	UpdatedAt       time.Time
}

type ProblemLanguageLimit struct {
	Language      string
	TimeLimitMs   int
	MemoryLimitMb int
}

type CreateProblemArg struct {
	ProblemNo       string
	Title           string
	Statement       string
	StatementFormat string
	Solution        string
	SolutionFormat  string
	ProblemType     string
	Visibility      string
	Difficulty      string
	Tags            []string
	TimeLimitMs     int
	MemoryLimitMb   int
	LanguageLimits  []ProblemLanguageLimit
	CreatedBy       int64
}

func (r *Repository) InsertProblem(ctx context.Context, arg CreateProblemArg) (int64, string, error) {
	var id int64
	statementFormat := strings.TrimSpace(arg.StatementFormat)
	if statementFormat == "" {
		statementFormat = "markdown+latex"
	}
	solutionFormat := strings.TrimSpace(arg.SolutionFormat)
	if solutionFormat == "" {
		solutionFormat = "markdown+latex"
	}

	err := r.db.QueryRow(
		ctx,
		`
INSERT INTO problems(
    problem_no,
    title,
    statement,
    statement_format,
    solution,
    solution_format,
    problem_type,
    visibility,
    time_limit_ms,
    memory_limit_mb,
    difficulty,
    tags,
    status,
    source_format,
    created_by,
    created_at,
    updated_at
)
VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'draft', 'ojos', $13, NOW(), NOW())
RETURNING id
`,
		arg.ProblemNo,
		arg.Title,
		arg.Statement,
		statementFormat,
		arg.Solution,
		solutionFormat,
		arg.ProblemType,
		arg.Visibility,
		arg.TimeLimitMs,
		arg.MemoryLimitMb,
		arg.Difficulty,
		arg.Tags,
		arg.CreatedBy,
	).Scan(&id)
	if err != nil {
		return 0, "", err
	}

	problemNo := strings.TrimSpace(arg.ProblemNo)
	if problemNo == "" {
		problemNo = fmt.Sprintf("P%d", id)
		if _, err := r.db.Exec(ctx, `UPDATE problems SET problem_no = $2 WHERE id = $1`, id, problemNo); err != nil {
			return 0, "", err
		}
	}
	if err := r.ReplaceProblemLanguageLimits(ctx, id, arg.LanguageLimits); err != nil {
		return 0, "", err
	}

	return id, problemNo, nil
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
	problemNo string,
	title string,
	statement string,
	solution string,
	problemType string,
	visibility string,
	status string,
	difficulty string,
	tags []string,
	timeLimitMs int,
	memoryLimitMb int,
	manifestSha string,
) error {
	_, err := r.db.Exec(
		ctx,
		`
UPDATE problems
SET
    problem_no = COALESCE(NULLIF($2, ''), problem_no),
    title = COALESCE(NULLIF($3, ''), title),
    statement = COALESCE(NULLIF($4, ''), statement),
    solution = COALESCE(NULLIF($5, ''), solution),
    problem_type = COALESCE(NULLIF($6, ''), problem_type),
    visibility = COALESCE(NULLIF($7, ''), visibility),
    status = COALESCE(NULLIF($8, ''), status),
    difficulty = COALESCE(NULLIF($9, ''), difficulty),
    tags = CASE WHEN $10::text[] IS NULL THEN tags ELSE $10::text[] END,
    time_limit_ms = CASE WHEN $11 > 0 THEN $11 ELSE time_limit_ms END,
    memory_limit_mb = CASE WHEN $12 > 0 THEN $12 ELSE memory_limit_mb END,
    manifest_sha256 = COALESCE(NULLIF($13, ''), manifest_sha256),
    updated_at = NOW()
WHERE id = $1
`,
		id,
		problemNo,
		title,
		statement,
		solution,
		problemType,
		visibility,
		status,
		difficulty,
		nullableTags(tags),
		timeLimitMs,
		memoryLimitMb,
		manifestSha,
	)
	return err
}

func (r *Repository) ReplaceProblemLanguageLimits(ctx context.Context, problemID int64, limits []ProblemLanguageLimit) error {
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return err
	}
	defer func() {
		_ = tx.Rollback(ctx)
	}()

	if _, err := tx.Exec(ctx, `DELETE FROM problem_language_limits WHERE problem_id = $1`, problemID); err != nil {
		return err
	}

	for _, limit := range limits {
		language := strings.ToLower(strings.TrimSpace(limit.Language))
		if language == "" {
			continue
		}
		_, err := tx.Exec(
			ctx,
			`
INSERT INTO problem_language_limits(
    problem_id,
    language,
    time_limit_ms,
    memory_limit_mb,
    created_at,
    updated_at
)
VALUES($1, $2, $3, $4, NOW(), NOW())
ON CONFLICT(problem_id, language)
DO UPDATE SET
    time_limit_ms = EXCLUDED.time_limit_ms,
    memory_limit_mb = EXCLUDED.memory_limit_mb,
    updated_at = NOW()
`,
			problemID,
			language,
			limit.TimeLimitMs,
			limit.MemoryLimitMb,
		)
		if err != nil {
			return err
		}
	}

	return tx.Commit(ctx)
}

func (r *Repository) GetProblem(ctx context.Context, id int64) (*Problem, error) {
	var p Problem

	err := r.db.QueryRow(
		ctx,
		`
SELECT
    id,
    COALESCE(problem_no, ''),
    COALESCE(slug, ''),
    title,
    COALESCE(statement, ''),
    COALESCE(statement_format, 'markdown+latex'),
    COALESCE(solution, ''),
    COALESCE(solution_format, 'markdown+latex'),
    COALESCE(problem_type, 'traditional'),
    COALESCE(visibility, 'private'),
    COALESCE(package_dir, ''),
    COALESCE(manifest_path, ''),
    COALESCE(manifest_sha256, ''),
    COALESCE(source_format, 'ojos'),
    COALESCE(status, 'draft'),
    COALESCE(difficulty, 'medium'),
    COALESCE(tags, '{}'::text[]),
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
		&p.ProblemNo,
		&p.Slug,
		&p.Title,
		&p.Statement,
		&p.StatementFormat,
		&p.Solution,
		&p.SolutionFormat,
		&p.ProblemType,
		&p.Visibility,
		&p.PackageDir,
		&p.ManifestPath,
		&p.ManifestSha256,
		&p.SourceFormat,
		&p.Status,
		&p.Difficulty,
		&p.Tags,
		&p.TimeLimitMs,
		&p.MemoryLimitMb,
		&p.CreatedBy,
		&p.CreatedAt,
		&p.UpdatedAt,
	)

	if err != nil {
		return nil, err
	}
	if err := r.attachLanguageLimits(ctx, []*Problem{&p}); err != nil {
		return nil, err
	}

	return &p, nil
}

type ListProblemsFilter struct {
	UserID         int64
	CanViewPrivate bool
	Page           int
	PageSize       int
	Keyword        string
	Visibility     string
	Difficulty     string
	Tags           []string
}

func (r *Repository) ListProblems(ctx context.Context, filter ListProblemsFilter) ([]Problem, int64, error) {
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

	offset := (page - 1) * pageSize

	where, args := buildProblemListWhere(filter)

	var total int64
	countSQL := `SELECT COUNT(*) FROM problems ` + where
	if err := r.db.QueryRow(ctx, countSQL, args...).Scan(&total); err != nil {
		return nil, 0, err
	}

	queryArgs := append([]any{}, args...)
	limitIndex := len(queryArgs) + 1
	offsetIndex := len(queryArgs) + 2
	queryArgs = append(queryArgs, pageSize, offset)

	rows, err := r.db.Query(
		ctx,
		fmt.Sprintf(`
SELECT
    id,
    COALESCE(problem_no, ''),
    COALESCE(slug, ''),
    title,
    COALESCE(statement, ''),
    COALESCE(statement_format, 'markdown+latex'),
    COALESCE(solution, ''),
    COALESCE(solution_format, 'markdown+latex'),
    COALESCE(problem_type, 'traditional'),
    COALESCE(visibility, 'private'),
    COALESCE(package_dir, ''),
    COALESCE(manifest_path, ''),
    COALESCE(manifest_sha256, ''),
    COALESCE(source_format, 'ojos'),
    COALESCE(status, 'draft'),
    COALESCE(difficulty, 'medium'),
    COALESCE(tags, '{}'::text[]),
    time_limit_ms,
    memory_limit_mb,
    COALESCE(created_by, 0),
    created_at,
    updated_at
FROM problems
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

	var problems []Problem
	for rows.Next() {
		var p Problem
		if err := rows.Scan(
			&p.ID,
			&p.ProblemNo,
			&p.Slug,
			&p.Title,
			&p.Statement,
			&p.StatementFormat,
			&p.Solution,
			&p.SolutionFormat,
			&p.ProblemType,
			&p.Visibility,
			&p.PackageDir,
			&p.ManifestPath,
			&p.ManifestSha256,
			&p.SourceFormat,
			&p.Status,
			&p.Difficulty,
			&p.Tags,
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
	ptrs := make([]*Problem, 0, len(problems))
	for i := range problems {
		ptrs = append(ptrs, &problems[i])
	}
	if err := r.attachLanguageLimits(ctx, ptrs); err != nil {
		return nil, 0, err
	}

	return problems, total, nil
}

func (r *Repository) GetProblemVisibleToUser(
	ctx context.Context,
	id int64,
	userID int64,
	canViewPrivate bool,
) (*Problem, error) {
	filter := ListProblemsFilter{
		UserID:         userID,
		CanViewPrivate: canViewPrivate,
	}
	where, args := buildProblemListWhere(filter)
	args = append(args, id)

	query := fmt.Sprintf(`
SELECT
    id,
    COALESCE(problem_no, ''),
    COALESCE(slug, ''),
    title,
    COALESCE(statement, ''),
    COALESCE(statement_format, 'markdown+latex'),
    COALESCE(solution, ''),
    COALESCE(solution_format, 'markdown+latex'),
    COALESCE(problem_type, 'traditional'),
    COALESCE(visibility, 'private'),
    COALESCE(package_dir, ''),
    COALESCE(manifest_path, ''),
    COALESCE(manifest_sha256, ''),
    COALESCE(source_format, 'ojos'),
    COALESCE(status, 'draft'),
    COALESCE(difficulty, 'medium'),
    COALESCE(tags, '{}'::text[]),
    time_limit_ms,
    memory_limit_mb,
    COALESCE(created_by, 0),
    created_at,
    updated_at
FROM problems
%s
  AND id = $%d
`, where, len(args))

	var p Problem
	err := r.db.QueryRow(ctx, query, args...).Scan(
		&p.ID,
		&p.ProblemNo,
		&p.Slug,
		&p.Title,
		&p.Statement,
		&p.StatementFormat,
		&p.Solution,
		&p.SolutionFormat,
		&p.ProblemType,
		&p.Visibility,
		&p.PackageDir,
		&p.ManifestPath,
		&p.ManifestSha256,
		&p.SourceFormat,
		&p.Status,
		&p.Difficulty,
		&p.Tags,
		&p.TimeLimitMs,
		&p.MemoryLimitMb,
		&p.CreatedBy,
		&p.CreatedAt,
		&p.UpdatedAt,
	)
	if err != nil {
		return nil, err
	}
	if err := r.attachLanguageLimits(ctx, []*Problem{&p}); err != nil {
		return nil, err
	}

	return &p, nil
}

func (r *Repository) CanViewPrivateProblems(ctx context.Context, userID int64) (bool, error) {
	return false, nil
}

func (r *Repository) attachLanguageLimits(ctx context.Context, problems []*Problem) error {
	if len(problems) == 0 {
		return nil
	}

	byID := make(map[int64]*Problem, len(problems))
	ids := make([]int64, 0, len(problems))
	for _, problem := range problems {
		if problem == nil {
			continue
		}
		byID[problem.ID] = problem
		ids = append(ids, problem.ID)
	}
	if len(ids) == 0 {
		return nil
	}

	rows, err := r.db.Query(
		ctx,
		`
SELECT problem_id, language, time_limit_ms, memory_limit_mb
FROM problem_language_limits
WHERE problem_id = ANY($1)
ORDER BY problem_id, language
`,
		ids,
	)
	if err != nil {
		return err
	}
	defer rows.Close()

	for rows.Next() {
		var problemID int64
		var limit ProblemLanguageLimit
		if err := rows.Scan(
			&problemID,
			&limit.Language,
			&limit.TimeLimitMs,
			&limit.MemoryLimitMb,
		); err != nil {
			return err
		}
		if problem := byID[problemID]; problem != nil {
			problem.LanguageLimits = append(problem.LanguageLimits, limit)
		}
	}
	return rows.Err()
}

func buildProblemListWhere(filter ListProblemsFilter) (string, []any) {
	args := []any{filter.UserID, filter.CanViewPrivate}
	clauses := []string{
		`(
    visibility = 'public'
    OR created_by = $1
    OR $2::boolean
)`,
	}

	if keyword := strings.TrimSpace(filter.Keyword); keyword != "" {
		args = append(args, "%"+keyword+"%")
		clauses = append(clauses, fmt.Sprintf(`(title ILIKE $%d OR slug ILIKE $%d OR problem_no ILIKE $%d)`, len(args), len(args), len(args)))
	}

	if visibility := strings.TrimSpace(filter.Visibility); visibility != "" {
		args = append(args, visibility)
		clauses = append(clauses, fmt.Sprintf(`visibility = $%d`, len(args)))
	}

	if difficulty := strings.TrimSpace(filter.Difficulty); difficulty != "" {
		args = append(args, difficulty)
		clauses = append(clauses, fmt.Sprintf(`difficulty = $%d`, len(args)))
	}

	if len(filter.Tags) > 0 {
		args = append(args, filter.Tags)
		clauses = append(clauses, fmt.Sprintf(`tags && $%d::text[]`, len(args)))
	}

	return "WHERE " + strings.Join(clauses, "\n  AND "), args
}

func nullableTags(tags []string) any {
	if tags == nil {
		return nil
	}
	return tags
}

func (r *Repository) BindProblemOwner(ctx context.Context, userID int64, problemID int64) error {
	return nil
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

	if _, err := tx.Exec(ctx, `DELETE FROM problem_files WHERE problem_id = $1`, problemID); err != nil {
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
    FROM problems
    WHERE created_by = $1
      AND id = $2
)
`,
		userID,
		problemID,
	).Scan(&ok)

	return ok, err
}
