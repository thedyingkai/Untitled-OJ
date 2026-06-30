package repository

import (
	"context"
	"errors"
	"fmt"

	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
)

var ErrUserExists = errors.New("user already exists")

type UserRepository struct {
	db *pgxpool.Pool
}

func NewUserRepository(db *pgxpool.Pool) *UserRepository {
	return &UserRepository{
		db: db,
	}
}

func (r *UserRepository) CreateUserWithDefaultRole(
	ctx context.Context,
	username string,
	email string,
	passwordHash string,
) (int64, error) {
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return 0, err
	}
	defer tx.Rollback(ctx)

	var emailValue any
	if email == "" {
		emailValue = nil
	} else {
		emailValue = email
	}

	var userID int64

	err = tx.QueryRow(
		ctx,
		`
		INSERT INTO users(username, email, password_hash)
		VALUES($1, $2, $3)
		RETURNING id
		`,
		username,
		emailValue,
		passwordHash,
	).Scan(&userID)

	if err != nil {
		var pgErr *pgconn.PgError
		if errors.As(err, &pgErr) && pgErr.Code == "23505" {
			return 0, ErrUserExists
		}

		return 0, err
	}

	var roleID int64

	err = tx.QueryRow(
		ctx,
		`
		SELECT id
		FROM roles
		WHERE name = $1
		`,
		"user",
	).Scan(&roleID)

	if err != nil {
		return 0, fmt.Errorf("default role not found: %w", err)
	}

	_, err = tx.Exec(
		ctx,
		`
		INSERT INTO user_roles(user_id, role_id)
		VALUES($1, $2)
		ON CONFLICT DO NOTHING
		`,
		userID,
		roleID,
	)

	if err != nil {
		return 0, err
	}

	if err := tx.Commit(ctx); err != nil {
		return 0, err
	}

	return userID, nil
}

var ErrUserNotFound = errors.New("user not found")

func (r *UserRepository) GetByUsername(ctx context.Context, username string) (id int64, passwordHash string, err error) {
	err = r.db.QueryRow(
		ctx,
		`
		SELECT id, password_hash
		FROM users
		WHERE username = $1
		`,
		username,
	).Scan(&id, &passwordHash)

	if err != nil {
		return 0, "", ErrUserNotFound
	}

	return id, passwordHash, nil
}

func (r *UserRepository) GetRolesByUserID(ctx context.Context, userID int64) ([]string, error) {
	rows, err := r.db.Query(
		ctx,
		`
		SELECT r.name
		FROM roles r
		INNER JOIN user_roles ur ON ur.role_id = r.id
		WHERE ur.user_id = $1
		ORDER BY r.id
		`,
		userID,
	)

	if err != nil {
		return nil, err
	}

	defer rows.Close()

	roles := make([]string, 0)

	for rows.Next() {
		var role string

		if err := rows.Scan(&role); err != nil {
			return nil, err
		}

		roles = append(roles, role)
	}

	if err := rows.Err(); err != nil {
		return nil, err
	}

	return roles, nil
}

func (r *UserRepository) GetPermissionCodesByUserID(ctx context.Context, userID int64) ([]string, error) {
	rows, err := r.db.Query(
		ctx,
		`
WITH is_super_admin AS (
    SELECT EXISTS (
        SELECT 1
        FROM user_roles ur
        JOIN roles r ON r.id = ur.role_id
        WHERE ur.user_id = $1
          AND r.name = 'super_admin'

        UNION ALL

        SELECT 1
        FROM role_bindings rb
        JOIN roles r ON r.id = rb.role_id
        WHERE rb.principal_type = 'user'
          AND rb.principal_id = $1
          AND rb.scope_type = 'system'
          AND rb.scope_id = 0
          AND r.name = 'super_admin'
          AND (rb.expires_at IS NULL OR rb.expires_at > NOW())
    ) AS ok
),
role_permissions_for_user AS (
    SELECT rp.permission_code
    FROM user_roles ur
    JOIN role_permissions rp ON rp.role_id = ur.role_id
    WHERE ur.user_id = $1

    UNION

    SELECT rp.permission_code
    FROM role_bindings rb
    JOIN role_permissions rp ON rp.role_id = rb.role_id
    WHERE rb.principal_type = 'user'
      AND rb.principal_id = $1
      AND (rb.expires_at IS NULL OR rb.expires_at > NOW())
),
direct_allow AS (
    SELECT permission_code
    FROM permission_assignments
    WHERE principal_type = 'user'
      AND principal_id = $1
      AND effect = 'allow'
      AND (expires_at IS NULL OR expires_at > NOW())
),
direct_deny AS (
    SELECT permission_code
    FROM permission_assignments
    WHERE principal_type = 'user'
      AND principal_id = $1
      AND effect = 'deny'
      AND (expires_at IS NULL OR expires_at > NOW())
),
effective AS (
    SELECT p.code AS permission_code
    FROM permissions p
    CROSS JOIN is_super_admin s
    WHERE s.ok

    UNION

    SELECT permission_code FROM role_permissions_for_user

    UNION

    SELECT permission_code FROM direct_allow
)
SELECT permission_code
FROM effective
WHERE permission_code NOT IN (SELECT permission_code FROM direct_deny)
ORDER BY permission_code
`,
		userID,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	permissions := make([]string, 0)
	for rows.Next() {
		var permission string
		if err := rows.Scan(&permission); err != nil {
			return nil, err
		}
		permissions = append(permissions, permission)
	}

	if err := rows.Err(); err != nil {
		return nil, err
	}

	return permissions, nil
}
