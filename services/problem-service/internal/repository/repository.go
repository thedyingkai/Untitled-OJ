package repository

import (
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"

	"ojos-problem-events/problemv1"
	"ojos-problem-service/internal/packagefs"
	"ojos-shared/eventing"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
)

type databaseExecutor interface {
	Exec(context.Context, string, ...any) (pgconn.CommandTag, error)
	Query(context.Context, string, ...any) (pgx.Rows, error)
	QueryRow(context.Context, string, ...any) pgx.Row
	Begin(context.Context) (pgx.Tx, error)
}

type Repository struct {
	db   databaseExecutor
	pool *pgxpool.Pool
}

func New(db *pgxpool.Pool) *Repository {
	return &Repository{db: db, pool: db}
}

const problemMutationAdvisoryNamespace int64 = 0x4f4a4f5350524f42

// ProblemMutationSession owns one pooled PostgreSQL connection for the whole
// package mutation. The session advisory lock and the final transaction run on
// this exact connection, so waiting for a lock can never consume one pool
// connection and then deadlock waiting for a second connection to commit.
type ProblemMutationSession struct {
	conn     *pgxpool.Conn
	repo     *Repository
	lockKey  int64
	released bool
}

// LockProblemMutation serializes every authoring mutation for one problem
// across all service processes. I/O is intentionally performed outside a DB
// transaction while this session lock remains held.
func (r *Repository) LockProblemMutation(ctx context.Context, problemID int64) (*ProblemMutationSession, error) {
	if r == nil || r.pool == nil {
		return nil, errors.New("problem repository does not expose a PostgreSQL pool")
	}
	if problemID <= 0 {
		return nil, errors.New("invalid problem id")
	}
	conn, err := r.pool.Acquire(ctx)
	if err != nil {
		return nil, err
	}
	// XOR with a fixed namespace is bijective for int64 IDs, avoiding lock-key
	// overlap with callers that use the raw problem ID.
	lockKey := problemID ^ problemMutationAdvisoryNamespace
	if _, err := conn.Exec(ctx, `SELECT pg_advisory_lock($1::bigint)`, lockKey); err != nil {
		// Cancellation can race with the server acquiring a session advisory
		// lock. Never return an ambiguously locked connection to the pool; closing
		// the physical session is the only proof that such a lock is gone.
		closeCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		raw := conn.Hijack()
		_ = raw.Close(closeCtx)
		return nil, err
	}
	return &ProblemMutationSession{
		conn:    conn,
		repo:    &Repository{db: conn},
		lockKey: lockKey,
	}, nil
}

func (s *ProblemMutationSession) Repository() *Repository {
	if s == nil {
		return nil
	}
	return s.repo
}

func (s *ProblemMutationSession) InTransaction(ctx context.Context, fn func(*Repository) error) error {
	if s == nil || s.repo == nil {
		return errors.New("problem mutation session is not configured")
	}
	return s.repo.InTransaction(ctx, fn)
}

// Close always unlocks with a fresh bounded context. If unlock cannot be
// proved, the connection is removed from the pool rather than returning a
// session-level advisory lock to an unrelated request.
func (s *ProblemMutationSession) Close() error {
	if s == nil || s.conn == nil || s.released {
		return nil
	}
	s.released = true
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	var unlocked bool
	err := s.conn.QueryRow(ctx, `SELECT pg_advisory_unlock($1::bigint)`, s.lockKey).Scan(&unlocked)
	if err != nil || !unlocked {
		raw := s.conn.Hijack()
		_ = raw.Close(ctx)
		if err != nil {
			return fmt.Errorf("release problem mutation advisory lock: %w", err)
		}
		return errors.New("release problem mutation advisory lock returned false")
	}
	s.conn.Release()
	return nil
}

// InTransaction gives all repository calls in fn the same PostgreSQL
// transaction. It is the required boundary for a domain mutation and its
// integration_outbox record.
func (r *Repository) InTransaction(ctx context.Context, fn func(*Repository) error) error {
	if r == nil || r.db == nil {
		return errors.New("problem repository is not configured")
	}
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return err
	}
	// The request context may be cancelled while the callback is running. Use a
	// fresh bounded context for the defensive rollback so pgx can finish (or
	// discard) the connection instead of leaving an open transaction pinned to
	// the pool.
	defer func() {
		rollbackCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = tx.Rollback(rollbackCtx)
	}()
	if err := fn(&Repository{db: tx}); err != nil {
		return err
	}
	return tx.Commit(ctx)
}

func (r *Repository) ReserveProblemID(ctx context.Context) (int64, error) {
	var id int64
	err := r.db.QueryRow(ctx, `SELECT nextval(pg_get_serial_sequence('problems', 'id'))`).Scan(&id)
	return id, err
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
		nonNilTags(arg.Tags),
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

// InsertProblemWithID is used by the package publication flow: it reserves an
// identifier before filesystem/storage I/O and makes the row visible only in
// the final transaction that also records the outbox event.
func (r *Repository) InsertProblemWithID(ctx context.Context, id int64, arg CreateProblemArg) error {
	if id <= 0 {
		return errors.New("invalid reserved problem id")
	}
	statementFormat := strings.TrimSpace(arg.StatementFormat)
	if statementFormat == "" {
		statementFormat = "markdown+latex"
	}
	solutionFormat := strings.TrimSpace(arg.SolutionFormat)
	if solutionFormat == "" {
		solutionFormat = "markdown+latex"
	}
	problemNo := strings.TrimSpace(arg.ProblemNo)
	if problemNo == "" {
		problemNo = fmt.Sprintf("P%d", id)
	}
	_, err := r.db.Exec(ctx, `
INSERT INTO problems(
    id,
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
VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'draft', 'ojos', $14, NOW(), NOW())
`, id, problemNo, arg.Title, arg.Statement, statementFormat, arg.Solution, solutionFormat, arg.ProblemType, arg.Visibility, arg.TimeLimitMs, arg.MemoryLimitMb, arg.Difficulty, nonNilTags(arg.Tags), arg.CreatedBy)
	if err != nil {
		return err
	}
	return r.ReplaceProblemLanguageLimits(ctx, id, arg.LanguageLimits)
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

func nonNilTags(tags []string) []string {
	if tags == nil {
		return []string{}
	}
	return tags
}

func (r *Repository) BindProblemOwner(ctx context.Context, userID int64, problemID int64) error {
	return nil
}

func (r *Repository) UpsertProblemFiles(ctx context.Context, problemID int64, files []packagefs.IndexedFile) error {
	// Keep replacement cleanup candidates and the new problem_files projection
	// in one transaction. When the caller already owns the domain transaction,
	// pgx implements this nested transaction as a savepoint, so a later outbox
	// or snapshot failure rolls both changes back together.
	return r.InTransaction(ctx, func(txRepo *Repository) error {
		for _, f := range files {
			var previousStoragePath string
			var previousSHA256 string
			var previousSizeBytes int64
			err := txRepo.db.QueryRow(ctx, `
SELECT storage_path, sha256, size_bytes
FROM problem_files
WHERE problem_id = $1 AND logical_path = $2
FOR UPDATE
`, problemID, f.LogicalPath).Scan(&previousStoragePath, &previousSHA256, &previousSizeBytes)
			if err != nil && !errors.Is(err, pgx.ErrNoRows) {
				return err
			}
			if err == nil && !sameProblemFileArtifactIdentity(previousStoragePath, previousSHA256, previousSizeBytes, f.StoragePath, f.Sha256, f.SizeBytes) {
				if err := txRepo.registerProblemFileCleanupCandidate(ctx, previousStoragePath, previousSHA256, previousSizeBytes); err != nil {
					return err
				}
			}

			_, err = txRepo.db.Exec(
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
	})
}

func (r *Repository) DeleteProblemFiles(ctx context.Context, problemID int64, logicalPaths []string) error {
	return r.InTransaction(ctx, func(txRepo *Repository) error {
		for _, logical := range logicalPaths {
			var storagePath string
			var sha256 string
			var sizeBytes int64
			err := txRepo.db.QueryRow(ctx, `
DELETE FROM problem_files
WHERE problem_id = $1 AND logical_path = $2
RETURNING storage_path, sha256, size_bytes
`,
				problemID,
				logical,
			).Scan(&storagePath, &sha256, &sizeBytes)
			if errors.Is(err, pgx.ErrNoRows) {
				continue
			}
			if err != nil {
				return err
			}
			if err := txRepo.registerProblemFileCleanupCandidate(ctx, storagePath, sha256, sizeBytes); err != nil {
				return err
			}
		}
		return nil
	})
}

func (r *Repository) DeleteProblem(ctx context.Context, problemID int64) error {
	return r.InTransaction(ctx, func(txRepo *Repository) error {
		rows, err := txRepo.db.Query(ctx, `
SELECT storage_path, sha256, size_bytes
FROM problem_files
WHERE problem_id = $1
FOR UPDATE
`, problemID)
		if err != nil {
			return err
		}
		type fileArtifact struct {
			storagePath string
			sha256      string
			sizeBytes   int64
		}
		var fileArtifacts []fileArtifact
		for rows.Next() {
			var artifact fileArtifact
			if err := rows.Scan(&artifact.storagePath, &artifact.sha256, &artifact.sizeBytes); err != nil {
				rows.Close()
				return err
			}
			fileArtifacts = append(fileArtifacts, artifact)
		}
		if err := rows.Err(); err != nil {
			rows.Close()
			return err
		}
		rows.Close()

		for _, artifact := range fileArtifacts {
			if err := txRepo.registerProblemFileCleanupCandidate(ctx, artifact.storagePath, artifact.sha256, artifact.sizeBytes); err != nil {
				return err
			}
		}
		if _, err := txRepo.db.Exec(ctx, `DELETE FROM problem_files WHERE problem_id = $1`, problemID); err != nil {
			return err
		}

		// Deliberately do not enqueue package_artifact_uri or historical
		// problem_package_revisions here. Judge submissions pin immutable package
		// revisions across the service boundary, so committed package ZIPs are
		// retained until a future cross-service retention protocol can prove them
		// unused. Failed, never-committed package uploads remain covered by the
		// existing upload-intent GC.
		tag, err := txRepo.db.Exec(ctx, `DELETE FROM problems WHERE id = $1`, problemID)
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return fmt.Errorf("problem not found: %d", problemID)
		}
		return nil
	})
}

func sameProblemFileArtifactIdentity(leftURI, leftSHA string, leftSize int64, rightURI, rightSHA string, rightSize int64) bool {
	return strings.TrimSpace(leftURI) == strings.TrimSpace(rightURI) &&
		strings.EqualFold(strings.TrimSpace(leftSHA), strings.TrimSpace(rightSHA)) &&
		leftSize == rightSize
}

// registerProblemFileCleanupCandidate records a previously committed content
// object before its problem_files reference is replaced or removed. It never
// overwrites an upload intent or an active GC claim for the same immutable URI;
// the collector's final reference check remains the authority on deletion.
// Invalid, local and package-archive paths are intentionally ignored.
func (r *Repository) registerProblemFileCleanupCandidate(ctx context.Context, storagePath, sha256 string, sizeBytes int64) error {
	artifact := problemv1.ArtifactRef{URI: storagePath, SHA256: sha256, SizeBytes: sizeBytes}
	uri, digest, err := normalizedArtifactIntent(artifact)
	if err != nil {
		return nil
	}
	trimmed := strings.TrimPrefix(uri, "storage://")
	separator := strings.IndexByte(trimmed, '/')
	if separator < 0 || !isProblemContentObjectKey(trimmed[separator+1:], digest) {
		return nil
	}
	_, err = r.db.Exec(ctx, `
INSERT INTO problem_artifact_upload_intents(
    artifact_uri, artifact_sha256, artifact_size_bytes, status,
    retry_after, upload_completed_at, created_at, updated_at
)
VALUES($1, $2, $3, 'PENDING', NOW(), NOW(), NOW(), NOW())
ON CONFLICT(artifact_uri) DO NOTHING
`, uri, digest, sizeBytes)
	return err
}

type ProjectionBackfillCandidate struct {
	ID                    int64
	PackageDir            string
	PackageArtifactSHA256 string
	HasCurrentOutbox      bool
}

var ErrProblemMutationConflict = errors.New("problem package mutation version conflict")
var ErrArtifactGCInProgress = errors.New("problem artifact is being reclaimed; retry the mutation")
var ErrArtifactNeedsAttention = errors.New("problem artifact requires operator attention before the mutation can continue")
var ErrArtifactIntentMissing = errors.New("problem artifact upload intent is missing")
var ErrArtifactIntentUnreferenced = errors.New("problem artifact upload intent has no matching committed reference")
var ErrArtifactUploadIncomplete = errors.New("problem artifact upload has not completed identity verification")

func normalizedArtifactIntent(artifact problemv1.ArtifactRef) (string, string, error) {
	uri := strings.TrimSpace(artifact.URI)
	digest := strings.TrimPrefix(strings.ToLower(strings.TrimSpace(artifact.SHA256)), "sha256:")
	storagePath := strings.TrimPrefix(uri, "storage://")
	separator := strings.IndexByte(storagePath, '/')
	validDigest := len(digest) == 64
	if validDigest {
		_, err := hex.DecodeString(digest)
		validDigest = err == nil
	}
	key := ""
	bucket := ""
	if separator >= 0 {
		bucket = storagePath[:separator]
		key = storagePath[separator+1:]
	}
	validSize := artifact.SizeBytes >= 0
	if isProblemPackageArtifactKey(key, digest) {
		validSize = artifact.SizeBytes > 0
	}
	if !strings.HasPrefix(uri, "storage://") || !validProblemArtifactBucket(bucket) ||
		!validProblemArtifactIntentKey(key, digest) || !validDigest || !validSize {
		return "", "", errors.New("remote problem artifact upload intent is invalid")
	}
	return uri, digest, nil
}

func validProblemArtifactBucket(bucket string) bool {
	if len(bucket) < 2 || len(bucket) > 63 {
		return false
	}
	first := bucket[0]
	if !((first >= 'a' && first <= 'z') || (first >= '0' && first <= '9')) {
		return false
	}
	for _, character := range bucket[1:] {
		if character >= 'a' && character <= 'z' || character >= '0' && character <= '9' || character == '.' || character == '-' {
			continue
		}
		return false
	}
	return true
}

func validProblemArtifactIntentKey(key, digest string) bool {
	if isProblemPackageArtifactKey(key, digest) {
		return true
	}
	return isProblemContentObjectKey(key, digest)
}

func isProblemPackageArtifactKey(key, digest string) bool {
	return key == "package-sha256-"+digest+".zip"
}

func isProblemContentObjectKey(key, digest string) bool {
	const prefix = "problem-"
	suffix := "-objects-sha256-" + digest
	if !strings.HasPrefix(key, prefix) || !strings.HasSuffix(key, suffix) {
		return false
	}
	problemIDText := strings.TrimSuffix(strings.TrimPrefix(key, prefix), suffix)
	problemID, err := strconv.ParseInt(problemIDText, 10, 64)
	return err == nil && problemID > 0 && strconv.FormatInt(problemID, 10) == problemIDText
}

// RegisterArtifactUploadIntent is intentionally committed before object I/O.
// It is safe to replay for the same immutable URI. An active GC claim owns the
// object exclusively; a publisher must retry after that bounded lease instead
// of racing a delete.
func (r *Repository) RegisterArtifactUploadIntent(ctx context.Context, artifact problemv1.ArtifactRef) error {
	uri, digest, err := normalizedArtifactIntent(artifact)
	if err != nil {
		return err
	}
	var registered string
	err = r.db.QueryRow(ctx, `
INSERT INTO problem_artifact_upload_intents(
    artifact_uri, artifact_sha256, artifact_size_bytes, status,
    retry_after, created_at, updated_at
)
VALUES($1, $2, $3, 'PENDING', NOW(), NOW(), NOW())
ON CONFLICT(artifact_uri) DO UPDATE
SET artifact_sha256 = EXCLUDED.artifact_sha256,
    artifact_size_bytes = EXCLUDED.artifact_size_bytes,
    status = 'PENDING',
    retry_after = NOW(),
    claim_token = NULL,
    claim_until = NULL,
    attempt_count = 0,
    failure_count = 0,
    last_error = '',
    needs_attention_at = NULL,
    last_operator_retry_reason = '',
    last_operator_retry_at = NULL,
    upload_completed_at = NULL,
    manual_reconcile_requested_at = NULL,
    last_failure_stage = '',
    last_failure_kind = '',
    last_failure_http_status = NULL,
    last_failure_provider_result = '',
    last_failure_deterministic = FALSE,
    updated_at = NOW()
WHERE problem_artifact_upload_intents.status = 'PENDING'
RETURNING artifact_uri
	`, uri, digest, artifact.SizeBytes).Scan(&registered)
	if errors.Is(err, pgx.ErrNoRows) {
		var status string
		if statusErr := r.db.QueryRow(ctx, `
SELECT status FROM problem_artifact_upload_intents WHERE artifact_uri=$1
`, uri).Scan(&status); statusErr != nil {
			if errors.Is(statusErr, pgx.ErrNoRows) {
				return ErrArtifactIntentMissing
			}
			return statusErr
		}
		if status == "NEEDS_ATTENTION" {
			return ErrArtifactNeedsAttention
		}
		return ErrArtifactGCInProgress
	}
	return err
}

// MarkArtifactUploadCompleted is called only after Storage has returned and
// the publisher has verified the exact SHA-256 and size. It is an identity and
// state CAS, so an operator/collector transition can never bless a different
// object or an upload that is still in flight.
func (r *Repository) MarkArtifactUploadCompleted(ctx context.Context, artifact problemv1.ArtifactRef) error {
	uri, digest, err := normalizedArtifactIntent(artifact)
	if err != nil {
		return err
	}
	tag, err := r.db.Exec(ctx, `
UPDATE problem_artifact_upload_intents
SET upload_completed_at = NOW()
WHERE artifact_uri = $1
  AND artifact_sha256 = $2
  AND artifact_size_bytes = $3
  AND status = 'PENDING'
  AND manual_reconcile_requested_at IS NULL
`, uri, digest, artifact.SizeBytes)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 1 {
		return nil
	}
	var status string
	if err := r.db.QueryRow(ctx, `SELECT status FROM problem_artifact_upload_intents WHERE artifact_uri=$1`, uri).Scan(&status); err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrArtifactIntentMissing
		}
		return err
	}
	if status == "DELETING" {
		return ErrArtifactGCInProgress
	}
	if status == "NEEDS_ATTENTION" {
		return ErrArtifactNeedsAttention
	}
	return ErrArtifactIntentUnreferenced
}

// ResolveArtifactUploadIntent must run in the same transaction as the exact
// problem_files or immutable revision reference and integration_outbox
// snapshot. Production mutations fail closed when the pre-upload PENDING row
// is absent, mismatched, or already owned by GC.
func (r *Repository) ResolveArtifactUploadIntent(ctx context.Context, artifact problemv1.ArtifactRef) error {
	return r.resolveArtifactUploadIntent(ctx, artifact, false)
}

// ResolveLegacyArtifactUploadIntent is the only expand-first exemption for
// imported/backfilled references that predate the upload-intent ledger. Online
// create/update mutations must use ResolveArtifactUploadIntent instead.
func (r *Repository) ResolveLegacyArtifactUploadIntent(ctx context.Context, artifact problemv1.ArtifactRef) error {
	return r.resolveArtifactUploadIntent(ctx, artifact, true)
}

func (r *Repository) resolveArtifactUploadIntent(ctx context.Context, artifact problemv1.ArtifactRef, allowMissingLegacy bool) error {
	uri := strings.TrimSpace(artifact.URI)
	if !strings.HasPrefix(uri, "storage://") {
		return nil
	}
	uri, digest, err := normalizedArtifactIntent(artifact)
	if err != nil {
		return err
	}
	var initialStatus string
	var intentDigest string
	var intentSize int64
	var uploadCompletedAt *time.Time
	if err := r.db.QueryRow(ctx, `
SELECT status, artifact_sha256, artifact_size_bytes, upload_completed_at
FROM problem_artifact_upload_intents
WHERE artifact_uri = $1
FOR UPDATE
`, uri).Scan(&initialStatus, &intentDigest, &intentSize, &uploadCompletedAt); err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			if allowMissingLegacy {
				return nil
			}
			return ErrArtifactIntentMissing
		}
		return err
	}
	if initialStatus == "DELETING" {
		return ErrArtifactGCInProgress
	}
	if initialStatus == "NEEDS_ATTENTION" {
		return ErrArtifactNeedsAttention
	}
	if initialStatus != "PENDING" || intentDigest != digest || intentSize != artifact.SizeBytes {
		return ErrArtifactIntentUnreferenced
	}
	if uploadCompletedAt == nil {
		return ErrArtifactUploadIncomplete
	}
	var removed string
	err = r.db.QueryRow(ctx, `
DELETE FROM problem_artifact_upload_intents i
WHERE i.artifact_uri = $1
  AND i.artifact_sha256 = $2
  AND i.artifact_size_bytes = $3
  AND i.status = 'PENDING'
  AND i.upload_completed_at IS NOT NULL
  AND (
      EXISTS (
          SELECT 1 FROM problems p
          WHERE p.package_artifact_uri = i.artifact_uri
            AND LOWER(p.package_artifact_sha256) = i.artifact_sha256
            AND p.package_artifact_size_bytes = i.artifact_size_bytes
      )
      OR EXISTS (
          SELECT 1 FROM problem_package_revisions r
          WHERE r.artifact_uri = i.artifact_uri
            AND LOWER(r.artifact_sha256) = i.artifact_sha256
            AND r.artifact_size_bytes = i.artifact_size_bytes
      )
      OR EXISTS (
          SELECT 1 FROM problem_files f
          WHERE f.storage_path = i.artifact_uri
            AND LOWER(f.sha256) = i.artifact_sha256
            AND f.size_bytes = i.artifact_size_bytes
      )
  )
RETURNING i.artifact_uri
`, uri, digest, artifact.SizeBytes).Scan(&removed)
	if err == nil {
		return nil
	}
	if !errors.Is(err, pgx.ErrNoRows) {
		return err
	}
	var status string
	if err := r.db.QueryRow(ctx, `
SELECT status
FROM problem_artifact_upload_intents
WHERE artifact_uri = $1
`, uri).Scan(&status); err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			if allowMissingLegacy {
				return nil
			}
			return ErrArtifactIntentMissing
		}
		return err
	}
	if status == "DELETING" {
		return ErrArtifactGCInProgress
	}
	if status == "NEEDS_ATTENTION" {
		return ErrArtifactNeedsAttention
	}
	return ErrArtifactIntentUnreferenced
}

// ResolveProblemFileUploadIntents removes the pre-upload ledger entries only
// after the surrounding business transaction has persisted exact
// problem_files references. A missing or mismatched reference fails the
// transaction closed and leaves the object reclaimable.
func (r *Repository) ResolveProblemFileUploadIntents(ctx context.Context, files []packagefs.IndexedFile) error {
	resolved := make(map[string]problemv1.ArtifactRef, len(files))
	for _, file := range files {
		if !strings.HasPrefix(strings.TrimSpace(file.StoragePath), "storage://") {
			continue
		}
		artifact := problemv1.ArtifactRef{
			URI:         file.StoragePath,
			SHA256:      file.Sha256,
			SizeBytes:   file.SizeBytes,
			ContentType: file.MimeType,
		}
		uri, digest, err := normalizedArtifactIntent(artifact)
		if err != nil {
			return fmt.Errorf("resolve problem file upload intent %s: %w", file.LogicalPath, err)
		}
		artifact.URI = uri
		artifact.SHA256 = digest
		if previous, ok := resolved[uri]; ok {
			if previous.SHA256 != digest || previous.SizeBytes != artifact.SizeBytes {
				return fmt.Errorf("resolve problem file upload intent %s: duplicate artifact URI has conflicting identity", file.LogicalPath)
			}
			// One immutable object can back multiple logical files. The exact
			// PENDING intent was already required and resolved by the first
			// reference in this same call; this is not a missing-intent exemption.
			continue
		}
		if err := r.ResolveArtifactUploadIntent(ctx, artifact); err != nil {
			return fmt.Errorf("resolve problem file upload intent %s: %w", file.LogicalPath, err)
		}
		resolved[uri] = artifact
	}
	return nil
}

type ProblemProjectionState struct {
	AggregateVersion      int64
	PackageArtifactSHA256 string
	HasCurrentOutbox      bool
}

func (r *Repository) ProblemProjectionState(ctx context.Context, problemID int64) (ProblemProjectionState, error) {
	var state ProblemProjectionState
	err := r.db.QueryRow(ctx, `
SELECT
    aggregate_version,
    COALESCE(package_artifact_sha256, ''),
    EXISTS (
        SELECT 1
        FROM integration_outbox o
        WHERE o.aggregate_type = 'problem'
          AND o.aggregate_id = 'problem/' || problems.id::text
          AND o.aggregate_version = problems.aggregate_version
    )
FROM problems
WHERE id = $1
`, problemID).Scan(&state.AggregateVersion, &state.PackageArtifactSHA256, &state.HasCurrentOutbox)
	return state, err
}

// ProblemSnapshotVersionMatches proves a historical commit even when the
// connection that issued COMMIT was lost and a later mutation has already
// advanced the current row. The append-only outbox is the durable commit
// witness for the exact aggregate version and immutable artifact.
func (r *Repository) ProblemSnapshotVersionMatches(ctx context.Context, problemID, aggregateVersion int64, artifactSHA256 string) (bool, error) {
	var matches bool
	err := r.db.QueryRow(ctx, `
SELECT EXISTS (
    SELECT 1
    FROM integration_outbox
    WHERE aggregate_type = 'problem'
      AND aggregate_id = 'problem/' || $1::bigint::text
      AND aggregate_version = $2
      AND event_type = $3
      AND LOWER(payload->'data'->'package_artifact'->>'sha256') = LOWER($4)
)
`, problemID, aggregateVersion, problemv1.SnapshotType, strings.TrimSpace(artifactSHA256)).Scan(&matches)
	return matches, err
}

func (r *Repository) ProblemDeletionVersionExists(ctx context.Context, problemID, aggregateVersion int64) (bool, error) {
	var exists bool
	err := r.db.QueryRow(ctx, `
SELECT EXISTS (
    SELECT 1
    FROM integration_outbox
    WHERE aggregate_type = 'problem'
      AND aggregate_id = 'problem/' || $1::bigint::text
      AND aggregate_version = $2
      AND event_type = $3
)
`, problemID, aggregateVersion, problemv1.DeletedType).Scan(&exists)
	return exists, err
}

func (r *Repository) ListProjectionBackfillCandidates(ctx context.Context, afterID int64, limit int) ([]ProjectionBackfillCandidate, error) {
	if limit <= 0 || limit > 500 {
		limit = 100
	}
	rows, err := r.db.Query(ctx, `
SELECT
    id,
    package_dir,
    package_artifact_sha256,
    EXISTS (
        SELECT 1
        FROM integration_outbox o
        WHERE o.aggregate_type = 'problem'
          AND o.aggregate_id = 'problem/' || problems.id::text
          AND o.aggregate_version = problems.aggregate_version
    ) AS has_current_outbox
FROM problems
WHERE id > $1
  AND package_dir <> ''
ORDER BY id
LIMIT $2
`, afterID, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var candidates []ProjectionBackfillCandidate
	for rows.Next() {
		var candidate ProjectionBackfillCandidate
		if err := rows.Scan(&candidate.ID, &candidate.PackageDir, &candidate.PackageArtifactSHA256, &candidate.HasCurrentOutbox); err != nil {
			return nil, err
		}
		candidates = append(candidates, candidate)
	}
	return candidates, rows.Err()
}

// PublishProblemSnapshot advances the source version and writes a full
// versioned snapshot to the outbox. The caller must invoke it on the repository
// passed to InTransaction after applying the matching domain mutation.
func (r *Repository) PublishProblemSnapshot(ctx context.Context, problemID int64, artifact problemv1.ArtifactRef) (problemv1.Snapshot, error) {
	state, err := r.ProblemProjectionState(ctx, problemID)
	if err != nil {
		return problemv1.Snapshot{}, err
	}
	return r.PublishProblemSnapshotCAS(ctx, problemID, state.AggregateVersion, artifact)
}

// PublishProblemSnapshotCAS prevents a snapshot built from an older authoring
// tree from committing after a newer mutation. Callers that mutate package
// state must pass the version observed while holding the problem advisory lock.
func (r *Repository) PublishProblemSnapshotCAS(ctx context.Context, problemID int64, expectedAggregateVersion int64, artifact problemv1.ArtifactRef) (problemv1.Snapshot, error) {
	if problemID <= 0 || strings.TrimSpace(artifact.URI) == "" || strings.TrimSpace(artifact.SHA256) == "" || artifact.SizeBytes <= 0 {
		return problemv1.Snapshot{}, errors.New("problem package artifact is incomplete")
	}
	if expectedAggregateVersion < 0 {
		return problemv1.Snapshot{}, errors.New("expected aggregate version must not be negative")
	}
	var aggregateVersion int64
	var packageRevision int64
	var manifestSHA string
	var updatedAt time.Time
	err := r.db.QueryRow(ctx, `
UPDATE problems
SET aggregate_version = aggregate_version + 1,
    package_revision = CASE
        WHEN package_revision = 0 OR package_artifact_sha256 <> $2 THEN package_revision + 1
        ELSE package_revision
    END,
    package_artifact_uri = $3,
    package_artifact_sha256 = $2,
    package_artifact_size_bytes = $4,
    updated_at = NOW()
WHERE id = $1 AND aggregate_version = $5
RETURNING aggregate_version, package_revision, manifest_sha256, updated_at
	`, problemID, strings.ToLower(strings.TrimSpace(artifact.SHA256)), artifact.URI, artifact.SizeBytes, expectedAggregateVersion).Scan(&aggregateVersion, &packageRevision, &manifestSHA, &updatedAt)
	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return problemv1.Snapshot{}, ErrProblemMutationConflict
		}
		return problemv1.Snapshot{}, err
	}
	if _, err := r.db.Exec(ctx, `
INSERT INTO problem_package_revisions(
    problem_id,
    package_revision,
    aggregate_version,
    artifact_uri,
    artifact_sha256,
    artifact_size_bytes,
    manifest_sha256,
    created_at
)
VALUES($1, $2, $3, $4, $5, $6, $7, NOW())
ON CONFLICT(problem_id, package_revision) DO NOTHING
`, problemID, packageRevision, aggregateVersion, artifact.URI, strings.ToLower(artifact.SHA256), artifact.SizeBytes, manifestSHA); err != nil {
		return problemv1.Snapshot{}, err
	}

	problem, err := r.GetProblem(ctx, problemID)
	if err != nil {
		return problemv1.Snapshot{}, err
	}
	snapshot := problemv1.Snapshot{
		ProblemID:          problem.ID,
		AggregateVersion:   aggregateVersion,
		PackageRevision:    packageRevision,
		ProblemNo:          problem.ProblemNo,
		Title:              problem.Title,
		ProblemType:        problem.ProblemType,
		Status:             problem.Status,
		Visibility:         problem.Visibility,
		CreatedBy:          problem.CreatedBy,
		TimeLimitMS:        problem.TimeLimitMs,
		MemoryLimitMB:      problem.MemoryLimitMb,
		ManifestSHA256:     manifestSHA,
		PackageArtifact:    artifact,
		SourceUpdatedAtUTC: updatedAt.UTC(),
	}
	event, err := problemv1.SnapshotCodec.NewEvent(ctx, "ojos://problem-service", "problem/"+strconv.FormatInt(problemID, 10), aggregateVersion, snapshot)
	if err != nil {
		return problemv1.Snapshot{}, err
	}
	if err := eventing.Enqueue(ctx, r.db, event); err != nil {
		return problemv1.Snapshot{}, err
	}
	if err := r.ResolveArtifactUploadIntent(ctx, artifact); err != nil {
		return problemv1.Snapshot{}, err
	}
	return snapshot, nil
}

// EnqueueProblemDeleted writes the tombstone before the row is removed. Both
// calls must share the same outer transaction.
func (r *Repository) EnqueueProblemDeleted(ctx context.Context, problemID int64) error {
	state, err := r.ProblemProjectionState(ctx, problemID)
	if err != nil {
		return err
	}
	return r.EnqueueProblemDeletedCAS(ctx, problemID, state.AggregateVersion)
}

func (r *Repository) EnqueueProblemDeletedCAS(ctx context.Context, problemID int64, expectedAggregateVersion int64) error {
	var aggregateVersion int64
	if err := r.db.QueryRow(ctx, `
UPDATE problems
SET aggregate_version = aggregate_version + 1, updated_at = NOW()
WHERE id = $1 AND aggregate_version = $2
RETURNING aggregate_version
	`, problemID, expectedAggregateVersion).Scan(&aggregateVersion); err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrProblemMutationConflict
		}
		return err
	}
	deleted := problemv1.Deleted{ProblemID: problemID, AggregateVersion: aggregateVersion}
	event, err := problemv1.DeletedCodec.NewEvent(ctx, "ojos://problem-service", "problem/"+strconv.FormatInt(problemID, 10), aggregateVersion, deleted)
	if err != nil {
		return err
	}
	return eventing.Enqueue(ctx, r.db, event)
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
