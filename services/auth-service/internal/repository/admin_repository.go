package repository

import (
	"context"
	"errors"
	"strings"
	"time"

	"ojos-shared/security/permission"

	"github.com/jackc/pgx/v5"
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

type ServicePermissionInput struct {
	Code        string
	Name        string
	Description string
}

type ServiceRoleBindingInput struct {
	Role        string
	Permissions []string
}

type ServiceIdentityInput struct {
	ServiceCode string
	AllowedAPIs []string
	Grants      []ServiceIdentityGrantInput
}

type ServiceIdentityGrantInput struct {
	APIID          string
	PermissionCode string
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

func (r *AdminRepository) RegisterServicePermissions(ctx context.Context, serviceCode string, permissions []ServicePermissionInput, bindings []ServiceRoleBindingInput, identity *ServiceIdentityInput) ([]string, error) {
	serviceCode = strings.TrimSpace(serviceCode)
	if serviceCode == "" {
		return nil, errors.New("service_code is required")
	}
	if len(permissions) == 0 && serviceIdentityEmpty(identity) {
		return nil, errors.New("permissions or service_identity are required")
	}

	tx, err := r.db.Begin(ctx)
	if err != nil {
		return nil, err
	}
	defer func() {
		_ = tx.Rollback(ctx)
	}()

	registered := make([]string, 0, len(permissions))
	known := make(map[string]bool, len(permissions))
	declaredCodes := make([]string, 0, len(permissions))
	for _, item := range permissions {
		code := strings.TrimSpace(item.Code)
		if code == "" {
			return nil, errors.New("permission code is required")
		}
		name := strings.TrimSpace(item.Name)
		if name == "" {
			name = code
		}
		description := strings.TrimSpace(item.Description)
		if _, err := tx.Exec(ctx, `
INSERT INTO permissions(code, service_code, name, description)
VALUES($1, $2, $3, $4)
ON CONFLICT(code)
DO UPDATE SET service_code = EXCLUDED.service_code, name = EXCLUDED.name, description = EXCLUDED.description
`, code, serviceCode, name, description); err != nil {
			return nil, err
		}
		registered = append(registered, code)
		known[code] = true
		declaredCodes = append(declaredCodes, code)
	}

	if len(declaredCodes) > 0 {
		if _, err := tx.Exec(ctx, `
DELETE FROM permissions
WHERE service_code = $1
  AND NOT (code = ANY($2::text[]))
`, serviceCode, declaredCodes); err != nil {
			return nil, err
		}
	}

	for _, binding := range bindings {
		roleName := strings.TrimSpace(binding.Role)
		if roleName == "" {
			return nil, errors.New("role is required")
		}
		roleID, err := ensureServiceRole(ctx, tx, serviceCode, roleName)
		if err != nil {
			return nil, err
		}
		if _, err := tx.Exec(ctx, `
DELETE FROM role_permissions
WHERE role_id = $1
  AND permission_code IN (
      SELECT code
      FROM permissions
      WHERE service_code = $2
  )
`, roleID, serviceCode); err != nil {
			return nil, err
		}
		for _, rawPermission := range binding.Permissions {
			permissionCode := strings.TrimSpace(rawPermission)
			if permissionCode == "" {
				continue
			}
			if !known[permissionCode] {
				return nil, errors.New("role binding references permission outside service release")
			}
			if _, err := tx.Exec(ctx, `
INSERT INTO role_permissions(role_id, permission_code)
VALUES($1, $2)
ON CONFLICT(role_id, permission_code)
DO NOTHING
`, roleID, permissionCode); err != nil {
				return nil, err
			}
		}
	}

	if !serviceIdentityEmpty(identity) {
		if err := r.registerServiceIdentity(ctx, tx, serviceCode, identity); err != nil {
			return nil, err
		}
	}

	if err := tx.Commit(ctx); err != nil {
		return nil, err
	}
	return registered, nil
}

func (r *AdminRepository) DeleteServicePermissions(ctx context.Context, serviceCode string) (int64, error) {
	serviceCode = strings.TrimSpace(serviceCode)
	if serviceCode == "" {
		return 0, errors.New("service_code is required")
	}
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return 0, err
	}
	defer func() {
		_ = tx.Rollback(ctx)
	}()
	if _, err := tx.Exec(ctx, `
DELETE FROM service_identities
WHERE service_code = $1
`, serviceCode); err != nil {
		return 0, err
	}
	tag, err := tx.Exec(ctx, `
DELETE FROM permissions
WHERE service_code = $1
`, serviceCode)
	if err != nil {
		return 0, err
	}
	if err := tx.Commit(ctx); err != nil {
		return 0, err
	}
	return tag.RowsAffected(), nil
}

func (r *AdminRepository) ServiceCallerCanUsePermission(ctx context.Context, serviceCode string, permissionCode string, apiID string) (bool, error) {
	serviceCode = strings.TrimSpace(serviceCode)
	permissionCode = strings.TrimSpace(permissionCode)
	apiID = strings.TrimSpace(apiID)
	if serviceCode == "" || permissionCode == "" {
		return false, nil
	}
	var ok bool
	err := r.db.QueryRow(ctx, `
SELECT EXISTS (
    SELECT 1
    FROM service_identities si
    JOIN service_permission_grants spg
      ON spg.caller_service_code = si.service_code
    JOIN permissions p
      ON p.code = spg.permission_code
    WHERE si.service_code = $1
      AND si.enabled
      AND spg.enabled
      AND spg.permission_code = $2
      AND ($3 = '' OR spg.api_id = $3)
)
`, serviceCode, permissionCode, apiID).Scan(&ok)
	return ok, err
}

func (r *AdminRepository) UserEffectivePermissions(ctx context.Context, userID int64, scopeType string, scopeID int64) ([]string, error) {
	if userID <= 0 {
		return nil, errors.New("user_id is required")
	}
	rows, err := r.db.Query(ctx, `
SELECT code
FROM permissions
ORDER BY service_code, code
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	candidates := make([]string, 0)
	for rows.Next() {
		var code string
		if err := rows.Scan(&code); err != nil {
			return nil, err
		}
		candidates = append(candidates, code)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	allowed := make([]string, 0, len(candidates))
	for _, code := range candidates {
		ok, err := hasUserPermissionForScope(ctx, r.db, userID, code, scopeType, scopeID)
		if err != nil {
			return nil, err
		}
		if ok {
			allowed = append(allowed, code)
		}
	}
	return allowed, nil
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

func serviceIdentityEmpty(identity *ServiceIdentityInput) bool {
	if identity == nil {
		return true
	}
	if strings.TrimSpace(identity.ServiceCode) != "" {
		return false
	}
	if len(identity.AllowedAPIs) > 0 || len(identity.Grants) > 0 {
		return false
	}
	return true
}

func (r *AdminRepository) registerServiceIdentity(ctx context.Context, tx pgx.Tx, serviceCode string, identity *ServiceIdentityInput) error {
	identityCode := strings.TrimSpace(identity.ServiceCode)
	if identityCode == "" {
		identityCode = serviceCode
	}
	if identityCode != serviceCode {
		return errors.New("service_identity service_name must match service_code")
	}
	allowedAPIs := make(map[string]bool, len(identity.AllowedAPIs))
	for _, rawAPI := range identity.AllowedAPIs {
		apiID := strings.TrimSpace(rawAPI)
		if apiID != "" {
			allowedAPIs[apiID] = true
		}
	}
	if _, err := tx.Exec(ctx, `
INSERT INTO service_identities(service_code, enabled, updated_at)
VALUES($1, TRUE, NOW())
ON CONFLICT(service_code)
DO UPDATE SET enabled = TRUE, updated_at = NOW()
`, serviceCode); err != nil {
		return err
	}
	if _, err := tx.Exec(ctx, `
DELETE FROM service_permission_grants
WHERE caller_service_code = $1
`, serviceCode); err != nil {
		return err
	}
	for _, grant := range identity.Grants {
		apiID := strings.TrimSpace(grant.APIID)
		permissionCode := strings.TrimSpace(grant.PermissionCode)
		if apiID == "" || permissionCode == "" {
			return errors.New("service_identity grant api_id and permission are required")
		}
		if len(allowedAPIs) > 0 && !allowedAPIs[apiID] {
			return errors.New("service_identity grant api_id must be declared in allowed_apis")
		}
		var providerService string
		if err := tx.QueryRow(ctx, `
SELECT service_code
FROM permissions
WHERE code = $1
`, permissionCode).Scan(&providerService); err != nil {
			if errors.Is(err, pgx.ErrNoRows) {
				return errors.New("service_identity grant references unknown permission")
			}
			return err
		}
		if _, err := tx.Exec(ctx, `
INSERT INTO service_permission_grants(caller_service_code, api_id, permission_code, provider_service_code, enabled, updated_at)
VALUES($1, $2, $3, $4, TRUE, NOW())
ON CONFLICT(caller_service_code, api_id, permission_code)
DO UPDATE SET provider_service_code = EXCLUDED.provider_service_code, enabled = TRUE, updated_at = NOW()
`, serviceCode, apiID, permissionCode, providerService); err != nil {
			return err
		}
	}
	return nil
}

func ensureServiceRole(ctx context.Context, tx pgx.Tx, serviceCode string, roleName string) (int64, error) {
	var roleID int64
	err := tx.QueryRow(ctx, `
INSERT INTO roles(name, service_code, description, is_system)
VALUES($1, $2, $3, FALSE)
ON CONFLICT(name)
DO UPDATE SET service_code = EXCLUDED.service_code
RETURNING id
`, roleName, serviceCode, "release role for "+serviceCode).Scan(&roleID)
	return roleID, err
}

func hasUserPermissionForScope(ctx context.Context, db *pgxpool.Pool, userID int64, permissionCode string, scopeType string, scopeID int64) (bool, error) {
	scopeType = strings.TrimSpace(scopeType)
	if scopeType == "" {
		scopeType = permission.ScopeSystem
	}
	return permission.HasUserPermission(
		ctx,
		db,
		userID,
		permissionCode,
		permission.Scope{Type: scopeType, ID: scopeID},
	)
}
