package repository

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
)

const initialSuperAdminBootstrapKey = "initial-super-admin"

var (
	ErrAdminBootstrapConsumed           = errors.New("initial administrator bootstrap already consumed")
	ErrAdminBootstrapAlreadyInitialized = errors.New("administrator identity already exists")
	ErrAdminBootstrapUserExists         = errors.New("bootstrap user already exists")
)

type AdminBootstrapRepository struct {
	db *pgxpool.Pool
}

func NewAdminBootstrapRepository(db *pgxpool.Pool) *AdminBootstrapRepository {
	return &AdminBootstrapRepository{db: db}
}

// ValidateState is a startup precondition used whenever bootstrap is enabled.
// It prevents Auth from exposing a bootstrap route backed by a missing or
// partially applied schema.
func (r *AdminBootstrapRepository) ValidateState(ctx context.Context) error {
	if r == nil || r.db == nil {
		return errors.New("admin bootstrap repository is unavailable")
	}
	var exists bool
	if err := r.db.QueryRow(ctx, `
SELECT EXISTS (
    SELECT 1
    FROM auth_bootstrap_state
    WHERE bootstrap_key = $1
)
`, initialSuperAdminBootstrapKey).Scan(&exists); err != nil {
		return err
	}
	if !exists {
		return errors.New("initial administrator bootstrap state is missing; run migrations")
	}
	return nil
}

// BootstrapAdmin creates the initial administrator and consumes the bootstrap
// marker in one serializable transaction. SELECT ... FOR UPDATE provides the
// single-winner guarantee for concurrent requests; the durable completed_at
// marker makes every later request fail even after process restarts.
func (r *AdminBootstrapRepository) BootstrapAdmin(
	ctx context.Context,
	username string,
	email string,
	passwordHash string,
) (int64, error) {
	if r == nil || r.db == nil {
		return 0, errors.New("admin bootstrap repository is unavailable")
	}

	tx, err := r.db.BeginTx(ctx, pgx.TxOptions{IsoLevel: pgx.Serializable})
	if err != nil {
		return 0, err
	}
	defer func() { _ = tx.Rollback(ctx) }()

	var completedAt sql.NullTime
	if err := tx.QueryRow(ctx, `
SELECT completed_at
FROM auth_bootstrap_state
WHERE bootstrap_key = $1
FOR UPDATE
`, initialSuperAdminBootstrapKey).Scan(&completedAt); err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return 0, fmt.Errorf("admin bootstrap state is missing; run migrations")
		}
		return 0, err
	}
	if completedAt.Valid {
		return 0, ErrAdminBootstrapConsumed
	}

	var existingAdministratorID int64
	err = tx.QueryRow(ctx, `
SELECT user_id
FROM (
    SELECT u.id AS user_id
    FROM users u
    JOIN user_roles ur ON ur.user_id = u.id
    JOIN roles r ON r.id = ur.role_id
    WHERE r.name = 'super_admin'

    UNION

    SELECT u.id AS user_id
    FROM users u
    JOIN role_bindings rb
      ON rb.principal_type = 'user'
     AND rb.principal_id = u.id
    JOIN roles r ON r.id = rb.role_id
    WHERE r.name = 'super_admin'
      AND rb.scope_type = 'system'
      AND rb.scope_id = 0
      AND (rb.expires_at IS NULL OR rb.expires_at > NOW())
) administrators
ORDER BY user_id
LIMIT 1
`).Scan(&existingAdministratorID)
	if err != nil && !errors.Is(err, pgx.ErrNoRows) {
		return 0, err
	}
	if err == nil {
		if _, err := tx.Exec(ctx, `
UPDATE auth_bootstrap_state
SET completed_at = NOW(), user_id = $2
WHERE bootstrap_key = $1
  AND completed_at IS NULL
`, initialSuperAdminBootstrapKey, existingAdministratorID); err != nil {
			return 0, err
		}
		if _, err := tx.Exec(ctx, `
INSERT INTO permission_audit_logs(
    actor_type, actor_id, action, target_type, target_id,
    role_name, scope_type, scope_id, effect, metadata
)
VALUES('bootstrap', 0, 'auth.bootstrap.detect_existing_admin', 'user', $1,
       'super_admin', 'system', 0, 'deny', $2::jsonb)
`, existingAdministratorID, `{"bootstrap_key":"initial-super-admin"}`); err != nil {
			return 0, err
		}
		if err := tx.Commit(ctx); err != nil {
			return 0, err
		}
		return 0, ErrAdminBootstrapAlreadyInitialized
	}

	var emailValue any
	if email == "" {
		emailValue = nil
	} else {
		emailValue = email
	}
	var userID int64
	if err := tx.QueryRow(ctx, `
INSERT INTO users(username, email, password_hash)
VALUES($1, $2, $3)
RETURNING id
`, username, emailValue, passwordHash).Scan(&userID); err != nil {
		var pgErr *pgconn.PgError
		if errors.As(err, &pgErr) && pgErr.Code == "23505" {
			return 0, ErrAdminBootstrapUserExists
		}
		return 0, err
	}

	result, err := tx.Exec(ctx, `
INSERT INTO user_roles(user_id, role_id)
SELECT $1, id
FROM roles
WHERE name IN ('user', 'super_admin')
ON CONFLICT DO NOTHING
`, userID)
	if err != nil {
		return 0, err
	}
	if result.RowsAffected() != 2 {
		return 0, fmt.Errorf("required user and super_admin roles are not installed")
	}

	result, err = tx.Exec(ctx, `
UPDATE auth_bootstrap_state
SET completed_at = NOW(), user_id = $2
WHERE bootstrap_key = $1
  AND completed_at IS NULL
`, initialSuperAdminBootstrapKey, userID)
	if err != nil {
		return 0, err
	}
	if result.RowsAffected() != 1 {
		return 0, ErrAdminBootstrapConsumed
	}

	if _, err := tx.Exec(ctx, `
INSERT INTO permission_audit_logs(
    actor_type,
    actor_id,
    action,
    target_type,
    target_id,
    role_name,
    scope_type,
    scope_id,
    effect,
    metadata
)
VALUES('bootstrap', 0, 'auth.bootstrap.initial_admin', 'user', $1, 'super_admin', 'system', 0, 'allow', $2::jsonb)
`, userID, `{"bootstrap_key":"initial-super-admin"}`); err != nil {
		return 0, err
	}

	if err := tx.Commit(ctx); err != nil {
		return 0, err
	}
	return userID, nil
}
