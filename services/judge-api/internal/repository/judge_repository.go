package repository

import (
	"context"
	"errors"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

var ErrSubmissionNotFound = errors.New("submission not found")

type Repository struct {
	db *pgxpool.Pool
}

func New(db *pgxpool.Pool) *Repository {
	return &Repository{
		db: db,
	}
}

func (r *Repository) CreateProblem(
	ctx context.Context,
	title string,
	timeLimitMs int,
	memoryLimitMb int,
) (int64, error) {
	var id int64

	err := r.db.QueryRow(
		ctx,
		`
		INSERT INTO problems(title, time_limit_ms, memory_limit_mb)
		VALUES($1, $2, $3)
		RETURNING id
		`,
		title,
		timeLimitMs,
		memoryLimitMb,
	).Scan(&id)

	return id, err
}

func (r *Repository) AddTestCase(
	ctx context.Context,
	problemID int64,
	input string,
	output string,
	score int,
) (int64, error) {
	var id int64

	err := r.db.QueryRow(
		ctx,
		`
		INSERT INTO test_cases(problem_id, input, output, score)
		VALUES($1, $2, $3, $4)
		RETURNING id
		`,
		problemID,
		input,
		output,
		score,
	).Scan(&id)

	return id, err
}

func (r *Repository) CreateSubmission(
	ctx context.Context,
	problemID int64,
	userID int64,
	language string,
	code string,
) (int64, error) {
	var id int64

	err := r.db.QueryRow(
		ctx,
		`
		INSERT INTO submissions(problem_id, user_id, language, code, status)
		VALUES($1, $2, $3, $4, 'PENDING')
		RETURNING id
		`,
		problemID,
		userID,
		language,
		code,
	).Scan(&id)

	return id, err
}

type SubmissionView struct {
	ID        int64
	ProblemID int64
	UserID    int64
	Language  string
	Status    string
	Score     int
	TimeMS    int
	MemoryKB  int
	Message   string
}

func (r *Repository) GetSubmission(ctx context.Context, id int64) (*SubmissionView, error) {
	var s SubmissionView
	var message *string

	err := r.db.QueryRow(
		ctx,
		`
		SELECT id, problem_id, user_id, language, status, score, time_ms, memory_kb, message
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

	return &s, nil
}

type SubmissionCaseView struct {
	ID           int64
	SubmissionID int64
	TestCaseID   int64
	Status       string
	TimeMS       int
	MemoryKB     int
	Message      string
}

func (r *Repository) GetSubmissionCases(ctx context.Context, submissionID int64) ([]SubmissionCaseView, error) {
	rows, err := r.db.Query(
		ctx,
		`
		SELECT id, submission_id, test_case_id, status, time_ms, memory_kb, message
		FROM submission_cases
		WHERE submission_id = $1
		ORDER BY id
		`,
		submissionID,
	)

	if err != nil {
		return nil, err
	}

	defer rows.Close()

	cases := make([]SubmissionCaseView, 0)

	for rows.Next() {
		var item SubmissionCaseView
		var message *string

		if err := rows.Scan(
			&item.ID,
			&item.SubmissionID,
			&item.TestCaseID,
			&item.Status,
			&item.TimeMS,
			&item.MemoryKB,
			&message,
		); err != nil {
			return nil, err
		}

		if message != nil {
			item.Message = *message
		}

		cases = append(cases, item)
	}

	if err := rows.Err(); err != nil {
		return nil, err
	}

	return cases, nil
}
