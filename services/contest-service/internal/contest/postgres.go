package contest

import (
	"context"
	"errors"
	"fmt"
	"strconv"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
	"ojos-shared/eventing"
	generated "ojos.local/gen/contest_service"
)

const ContestCreatedEventType = generated.ContestServiceContestCreatedV1Type
const contestCreatedSchema = "urn:ojos:event:contest-service.contest-created:v1"

var contestCreatedCodec = eventing.MustCodec[CreatedV1](eventing.EventDescriptor{
	Type: ContestCreatedEventType, DataSchema: contestCreatedSchema,
}, nil, func(_ eventing.Envelope, value CreatedV1) error {
	if value.ContestID < 1 || value.Slug == "" || value.Title == "" || value.StartsAt.IsZero() || !value.EndsAt.After(value.StartsAt) {
		return ErrInvalid
	}
	return nil
})

type PostgresRepository struct{ pool *pgxpool.Pool }

func NewPostgresRepository(pool *pgxpool.Pool) (*PostgresRepository, error) {
	if pool == nil {
		return nil, errors.New("contest database is required")
	}
	return &PostgresRepository{pool: pool}, nil
}

func (repository *PostgresRepository) Ping(ctx context.Context) error {
	return repository.pool.Ping(ctx)
}

func scanContest(row pgx.Row) (Contest, error) {
	var item Contest
	err := row.Scan(&item.ID, &item.Slug, &item.Title, &item.Description, &item.StartsAt, &item.EndsAt, &item.Version, &item.CreatedAt, &item.UpdatedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return Contest{}, ErrNotFound
	}
	return item, err
}

const contestColumns = `id, slug, title, description, starts_at, ends_at, aggregate_version, created_at, updated_at`

func (repository *PostgresRepository) List(ctx context.Context) ([]Contest, error) {
	rows, err := repository.pool.Query(ctx, `SELECT `+contestColumns+` FROM contests ORDER BY starts_at, id`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]Contest, 0)
	for rows.Next() {
		item, scanErr := scanContest(rows)
		if scanErr != nil {
			return nil, scanErr
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (repository *PostgresRepository) Get(ctx context.Context, id int64) (Contest, error) {
	return scanContest(repository.pool.QueryRow(ctx, `SELECT `+contestColumns+` FROM contests WHERE id=$1`, id))
}

func (repository *PostgresRepository) Create(ctx context.Context, input CreateInput) (Contest, error) {
	if err := input.NormalizeAndValidate(); err != nil {
		return Contest{}, err
	}
	tx, err := repository.pool.BeginTx(ctx, pgx.TxOptions{})
	if err != nil {
		return Contest{}, err
	}
	defer func() { _ = tx.Rollback(ctx) }()
	item, err := scanContest(tx.QueryRow(ctx, `
INSERT INTO contests(slug, title, description, starts_at, ends_at)
VALUES($1, $2, $3, $4, $5)
RETURNING `+contestColumns, input.Slug, input.Title, input.Description, input.StartsAt, input.EndsAt))
	if err != nil {
		return Contest{}, mapPostgresError(err)
	}
	event, err := contestCreatedCodec.NewEvent(ctx, "urn:ojos:contest-service", "contest/"+strconv.FormatInt(item.ID, 10), item.Version, CreatedV1{
		ContestID: item.ID, Slug: item.Slug, Title: item.Title, StartsAt: item.StartsAt, EndsAt: item.EndsAt,
	})
	if err != nil {
		return Contest{}, err
	}
	if err := eventing.Enqueue(ctx, tx, event); err != nil {
		return Contest{}, fmt.Errorf("enqueue contest-created event: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return Contest{}, err
	}
	return item, nil
}

func (repository *PostgresRepository) Update(ctx context.Context, id int64, input UpdateInput) (Contest, error) {
	if err := input.NormalizeAndValidate(); err != nil {
		return Contest{}, err
	}
	item, err := scanContest(repository.pool.QueryRow(ctx, `
UPDATE contests
SET title=$2, description=$3, starts_at=$4, ends_at=$5,
    aggregate_version=aggregate_version+1, updated_at=NOW()
WHERE id=$1 AND aggregate_version=$6
RETURNING `+contestColumns, id, input.Title, input.Description, input.StartsAt, input.EndsAt, input.Version))
	if errors.Is(err, ErrNotFound) {
		if _, getErr := repository.Get(ctx, id); getErr == nil {
			return Contest{}, ErrConflict
		}
	}
	return item, mapPostgresError(err)
}

func (repository *PostgresRepository) Delete(ctx context.Context, id int64) error {
	tag, err := repository.pool.Exec(ctx, `DELETE FROM contests WHERE id=$1`, id)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return nil
}

func mapPostgresError(err error) error {
	if err == nil || errors.Is(err, ErrNotFound) || errors.Is(err, ErrInvalid) || errors.Is(err, ErrConflict) {
		return err
	}
	var postgresError *pgconn.PgError
	if errors.As(err, &postgresError) && postgresError.Code == "23505" {
		return ErrConflict
	}
	return err
}
