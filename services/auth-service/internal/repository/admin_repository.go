package repository

import (
	"context"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
)

type AdminRepository struct {
	db *pgxpool.Pool
}

func NewAdminRepository(db *pgxpool.Pool) *AdminRepository {
	return &AdminRepository{db: db}
}

type UserListItem struct {
	UserID    int64
	Username  string
	Email     string
	Roles     []string
	CreatedAt time.Time
}

type RoleListItem struct {
	ID          int64
	Name        string
	ServiceCode string
	Description string
	IsSystem    bool
}

type PermissionListItem struct {
	Code        string
	ServiceCode string
	Name        string
	Description string
}

type AuditListItem struct {
	ID             int64
	ActorType      string
	ActorID        int64
	Action         string
	TargetType     string
	TargetID       int64
	PermissionCode string
	RoleName       string
	ScopeType      string
	ScopeID        int64
	Effect         string
	CreatedAt      time.Time
}

func (r *AdminRepository) ListUsers(ctx context.Context) ([]UserListItem, error) {
	rows, err := r.db.Query(ctx, `
SELECT
    u.id,
    u.username,
    COALESCE(u.email, ''),
    COALESCE(array_agg(r.name ORDER BY r.name) FILTER (WHERE r.name IS NOT NULL), '{}'::text[]),
    u.created_at
FROM users u
LEFT JOIN user_roles ur ON ur.user_id = u.id
LEFT JOIN roles r ON r.id = ur.role_id
GROUP BY u.id, u.username, u.email, u.created_at
ORDER BY u.id DESC
LIMIT 500
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	items := make([]UserListItem, 0)
	for rows.Next() {
		var item UserListItem
		if err := rows.Scan(&item.UserID, &item.Username, &item.Email, &item.Roles, &item.CreatedAt); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (r *AdminRepository) ListRoles(ctx context.Context) ([]RoleListItem, error) {
	rows, err := r.db.Query(ctx, `
SELECT id, name, service_code, description, is_system
FROM roles
ORDER BY service_code, name
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	items := make([]RoleListItem, 0)
	for rows.Next() {
		var item RoleListItem
		if err := rows.Scan(&item.ID, &item.Name, &item.ServiceCode, &item.Description, &item.IsSystem); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (r *AdminRepository) ListPermissions(ctx context.Context) ([]PermissionListItem, error) {
	rows, err := r.db.Query(ctx, `
SELECT code, service_code, name, description
FROM permissions
ORDER BY service_code, code
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	items := make([]PermissionListItem, 0)
	for rows.Next() {
		var item PermissionListItem
		if err := rows.Scan(&item.Code, &item.ServiceCode, &item.Name, &item.Description); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (r *AdminRepository) AddGlobalUserRole(ctx context.Context, actorID int64, userID int64, roleName string) error {
	_, err := r.db.Exec(ctx, `
WITH inserted AS (
    INSERT INTO user_roles(user_id, role_id)
    SELECT $2, id
    FROM roles
    WHERE name = $3
    ON CONFLICT DO NOTHING
    RETURNING role_id
),
role_row AS (
    SELECT id, name
    FROM roles
    WHERE name = $3
)
INSERT INTO permission_audit_logs(actor_type, actor_id, action, target_type, target_id, role_id, role_name, scope_type, scope_id)
SELECT 'user', $1, 'role.bind', 'user', $2, id, name, 'system', 0
FROM role_row
WHERE EXISTS (SELECT 1 FROM inserted)
`, actorID, userID, roleName)
	return err
}

func (r *AdminRepository) EnsureGlobalUserRole(ctx context.Context, userID int64, roleName string) error {
	_, err := r.db.Exec(ctx, `
INSERT INTO user_roles(user_id, role_id)
SELECT $1, id
FROM roles
WHERE name = $2
ON CONFLICT DO NOTHING
`, userID, roleName)
	return err
}

func (r *AdminRepository) RemoveGlobalUserRole(ctx context.Context, actorID int64, userID int64, roleName string) error {
	_, err := r.db.Exec(ctx, `
WITH deleted AS (
    DELETE FROM user_roles ur
    USING roles r
    WHERE ur.role_id = r.id
      AND ur.user_id = $2
      AND r.name = $3
    RETURNING r.id, r.name
)
INSERT INTO permission_audit_logs(actor_type, actor_id, action, target_type, target_id, role_id, role_name, scope_type, scope_id)
SELECT 'user', $1, 'role.unbind', 'user', $2, id, name, 'system', 0
FROM deleted
`, actorID, userID, roleName)
	return err
}

func (r *AdminRepository) RemoveScopedRole(ctx context.Context, actorID int64, userID int64, roleName string, scopeType string, scopeID int64) error {
	_, err := r.db.Exec(ctx, `
WITH deleted AS (
    DELETE FROM role_bindings rb
    USING roles r
    WHERE rb.role_id = r.id
      AND rb.principal_type = 'user'
      AND rb.principal_id = $2
      AND r.name = $3
      AND rb.scope_type = $4
      AND rb.scope_id = $5
    RETURNING r.id, r.name
)
INSERT INTO permission_audit_logs(actor_type, actor_id, action, target_type, target_id, role_id, role_name, scope_type, scope_id)
SELECT 'user', $1, 'role.unbind', 'user', $2, id, name, $4, $5
FROM deleted
`, actorID, userID, roleName, scopeType, scopeID)
	return err
}

func (r *AdminRepository) ListAuditLogs(ctx context.Context) ([]AuditListItem, error) {
	rows, err := r.db.Query(ctx, `
SELECT
    id,
    actor_type,
    actor_id,
    action,
    target_type,
    target_id,
    permission_code,
    role_name,
    scope_type,
    scope_id,
    effect,
    created_at
FROM permission_audit_logs
ORDER BY id DESC
LIMIT 200
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	items := make([]AuditListItem, 0)
	for rows.Next() {
		var item AuditListItem
		if err := rows.Scan(
			&item.ID,
			&item.ActorType,
			&item.ActorID,
			&item.Action,
			&item.TargetType,
			&item.TargetID,
			&item.PermissionCode,
			&item.RoleName,
			&item.ScopeType,
			&item.ScopeID,
			&item.Effect,
			&item.CreatedAt,
		); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}
