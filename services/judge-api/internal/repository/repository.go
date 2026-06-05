package repository

import (
	"context"
	"errors"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

var ErrSubmissionNotFound = errors.New("submission not found")

type Repository struct {
	db *pgxpool.Pool
}

func New(db *pgxpool.Pool) *Repository {
	return &Repository{db: db}
}

type ProblemMeta struct {
	ID         int64
	PackageDir string
	Status     string
}

func (r *Repository) GetProblemMeta(ctx context.Context, id int64) (*ProblemMeta, error) {
	var p ProblemMeta

	err := r.db.QueryRow(
		ctx,
		`
SELECT
    id,
    package_dir,
    status
FROM problems
WHERE id = $1
`,
		id,
	).Scan(&p.ID, &p.PackageDir, &p.Status)

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
    updated_at
)
VALUES($1, $2, $3, 'PENDING', 0, 0, 0, '', NOW(), NOW())
RETURNING id
`,
		problemID,
		userID,
		language,
	).Scan(&id)

	return id, err
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
	ID           int64
	ProblemID    int64
	UserID       int64
	Language     string
	Status       string
	Score        int
	TimeMS       int
	MemoryKB     int
	Message      string
	CodePath     string
	CodeSha256   string
	ResultPath   string
	JudgedAt     *time.Time
	CancelledAt  *time.Time
	CancelReason string
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
    judged_at,
    cancelled_at,
    cancel_reason
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
		&s.JudgedAt,
		&s.CancelledAt,
		&cancelReason,
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
