package repository

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
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

type ResourceTypeListItem struct {
	Code        string
	ServiceCode string
	Name        string
	Description string
	CreatedAt   string
}

type RoleBindingListItem struct {
	ID            int64
	PrincipalType string
	PrincipalID   int64
	Role          string
	ScopeType     string
	ScopeID       int64
	GrantedByType string
	GrantedByID   int64
	ExpiresAt     string
	CreatedAt     string
}

type PermissionAssignmentListItem struct {
	ID             int64
	PrincipalType  string
	PrincipalID    int64
	PermissionCode string
	ScopeType      string
	ScopeID        int64
	Effect         string
	Reason         string
	GrantedByType  string
	GrantedByID    int64
	ExpiresAt      string
	CreatedAt      string
}

type ResourceEdgeListItem struct {
	ID         int64
	ParentType string
	ParentID   int64
	ChildType  string
	ChildID    int64
	Relation   string
	CreatedAt  string
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
	ServiceCode         string
	AllowedAPIs         []string
	Grants              []ServiceIdentityGrantInput
	CredentialToken     string
	CredentialExpiresAt *time.Time
}

type ServiceIdentityGrantInput struct {
	APIID          string
	PermissionCode string
}

type ServiceCredentialInput struct {
	Token     string
	ExpiresAt *time.Time
}

type ServiceCredentialListItem struct {
	ServiceCode string
	TokenHint   string
	Enabled     bool
	CreatedAt   string
	UpdatedAt   string
	ExpiresAt   string
	RevokedAt   string
	LastUsedAt  string
}

type ServiceGrantListItem struct {
	APIID               string
	PermissionCode      string
	ProviderServiceCode string
	Enabled             bool
}

type ServiceIdentityDetails struct {
	ServiceCode string
	Enabled     bool
	Grants      []ServiceGrantListItem
	Credentials []ServiceCredentialListItem
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

func (r *AdminRepository) ListResourceTypes(ctx context.Context) ([]ResourceTypeListItem, error) {
	rows, err := r.db.Query(ctx, `
SELECT
    code,
    service_code,
    name,
    description,
    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')
FROM resource_types
ORDER BY service_code, code
LIMIT 500
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]ResourceTypeListItem, 0)
	for rows.Next() {
		var item ResourceTypeListItem
		if err := rows.Scan(&item.Code, &item.ServiceCode, &item.Name, &item.Description, &item.CreatedAt); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (r *AdminRepository) ListRoleBindings(ctx context.Context) ([]RoleBindingListItem, error) {
	rows, err := r.db.Query(ctx, `
SELECT
    rb.id,
    rb.principal_type,
    rb.principal_id,
    r.name,
    rb.scope_type,
    rb.scope_id,
    rb.granted_by_type,
    rb.granted_by_id,
    COALESCE(to_char(rb.expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'), ''),
    to_char(rb.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')
FROM role_bindings rb
JOIN roles r ON r.id = rb.role_id
ORDER BY rb.id DESC
LIMIT 500
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]RoleBindingListItem, 0)
	for rows.Next() {
		var item RoleBindingListItem
		if err := rows.Scan(&item.ID, &item.PrincipalType, &item.PrincipalID, &item.Role, &item.ScopeType, &item.ScopeID, &item.GrantedByType, &item.GrantedByID, &item.ExpiresAt, &item.CreatedAt); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (r *AdminRepository) ListPermissionAssignments(ctx context.Context) ([]PermissionAssignmentListItem, error) {
	rows, err := r.db.Query(ctx, `
SELECT
    id,
    principal_type,
    principal_id,
    permission_code,
    scope_type,
    scope_id,
    effect,
    reason,
    granted_by_type,
    granted_by_id,
    COALESCE(to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'), ''),
    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')
FROM permission_assignments
ORDER BY id DESC
LIMIT 500
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]PermissionAssignmentListItem, 0)
	for rows.Next() {
		var item PermissionAssignmentListItem
		if err := rows.Scan(&item.ID, &item.PrincipalType, &item.PrincipalID, &item.PermissionCode, &item.ScopeType, &item.ScopeID, &item.Effect, &item.Reason, &item.GrantedByType, &item.GrantedByID, &item.ExpiresAt, &item.CreatedAt); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (r *AdminRepository) ListResourceEdges(ctx context.Context) ([]ResourceEdgeListItem, error) {
	rows, err := r.db.Query(ctx, `
SELECT
    id,
    parent_type,
    parent_id,
    child_type,
    child_id,
    relation,
    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')
FROM resource_edges
ORDER BY id DESC
LIMIT 500
`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]ResourceEdgeListItem, 0)
	for rows.Next() {
		var item ResourceEdgeListItem
		if err := rows.Scan(&item.ID, &item.ParentType, &item.ParentID, &item.ChildType, &item.ChildID, &item.Relation, &item.CreatedAt); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (r *AdminRepository) UpsertRole(ctx context.Context, actorID int64, name string, serviceCode string, description string, isSystem bool) error {
	name = strings.TrimSpace(name)
	serviceCode = strings.TrimSpace(serviceCode)
	if serviceCode == "" {
		serviceCode = "core"
	}
	description = strings.TrimSpace(description)
	if name == "" {
		return errors.New("role name is required")
	}
	_, err := r.db.Exec(ctx, `
INSERT INTO roles(name, service_code, description, is_system)
VALUES($1, $2, $3, $4)
ON CONFLICT(name)
DO UPDATE SET service_code = EXCLUDED.service_code,
              description = EXCLUDED.description,
              is_system = EXCLUDED.is_system
`, name, serviceCode, description, isSystem)
	if err != nil {
		return err
	}
	return permission.WriteAuditLog(ctx, r.db, permission.UserPrincipal(actorID), "role.upsert", permission.Principal{Type: "role", ID: 0}, "", 0, name, permission.SystemScope(), "", map[string]any{
		"role":         name,
		"service_code": serviceCode,
		"is_system":    isSystem,
	})
}

func (r *AdminRepository) DeleteRole(ctx context.Context, actorID int64, name string) error {
	name = strings.TrimSpace(name)
	if name == "" {
		return errors.New("role name is required")
	}
	tag, err := r.db.Exec(ctx, `
DELETE FROM roles
WHERE name = $1
  AND is_system = FALSE
`, name)
	if err != nil {
		return err
	}
	return permission.WriteAuditLog(ctx, r.db, permission.UserPrincipal(actorID), "role.delete", permission.Principal{Type: "role", ID: 0}, "", 0, name, permission.SystemScope(), "", map[string]any{
		"role": name,
		"rows": tag.RowsAffected(),
	})
}

func (r *AdminRepository) DeletePermission(ctx context.Context, actorID int64, code string) error {
	code = strings.TrimSpace(code)
	if code == "" {
		return errors.New("permission code is required")
	}
	tag, err := r.db.Exec(ctx, `
DELETE FROM permissions
WHERE code = $1
`, code)
	if err != nil {
		return err
	}
	return permission.WriteAuditLog(ctx, r.db, permission.UserPrincipal(actorID), "permission.delete", permission.Principal{Type: "permission", ID: 0}, code, 0, "", permission.SystemScope(), "", map[string]any{
		"permission": code,
		"rows":       tag.RowsAffected(),
	})
}

func (r *AdminRepository) DeleteResourceType(ctx context.Context, actorID int64, code string) error {
	code = strings.TrimSpace(code)
	if code == "" {
		return errors.New("resource type code is required")
	}
	tag, err := r.db.Exec(ctx, `
DELETE FROM resource_types
WHERE code = $1
`, code)
	if err != nil {
		return err
	}
	return permission.WriteAuditLog(ctx, r.db, permission.UserPrincipal(actorID), "resource_type.delete", permission.Principal{Type: "resource_type", ID: 0}, "", 0, "", permission.SystemScope(), "", map[string]any{
		"resource_type": code,
		"rows":          tag.RowsAffected(),
	})
}

func (r *AdminRepository) RegisterServicePermissions(ctx context.Context, actorID int64, serviceCode string, permissions []ServicePermissionInput, bindings []ServiceRoleBindingInput, identity *ServiceIdentityInput) ([]string, error) {
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
		if err := r.registerServiceIdentity(ctx, tx, actorID, serviceCode, identity); err != nil {
			return nil, err
		}
	}

	if err := writeAuditTx(ctx, tx, actorID, "service.permissions.register", serviceCode, "", "", "", map[string]any{
		"permissions": registered,
		"bindings":    len(bindings),
	}); err != nil {
		return nil, err
	}

	if err := tx.Commit(ctx); err != nil {
		return nil, err
	}
	return registered, nil
}

func (r *AdminRepository) DeleteServicePermissions(ctx context.Context, actorID int64, serviceCode string) (int64, error) {
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
	if err := writeAuditTx(ctx, tx, actorID, "service.permissions.delete", serviceCode, "", "", "", map[string]any{
		"deleted": tag.RowsAffected(),
	}); err != nil {
		return 0, err
	}
	if err := tx.Commit(ctx); err != nil {
		return 0, err
	}
	return tag.RowsAffected(), nil
}

func (r *AdminRepository) ServiceCallerCanUsePermission(ctx context.Context, serviceCode string, permissionCode string, apiID string, token string) (bool, error) {
	serviceCode = strings.TrimSpace(serviceCode)
	permissionCode = strings.TrimSpace(permissionCode)
	apiID = strings.TrimSpace(apiID)
	tokenHash := serviceCredentialTokenHash(token)
	if serviceCode == "" || permissionCode == "" || tokenHash == "" {
		return false, nil
	}
	var ok bool
	err := r.db.QueryRow(ctx, `
SELECT EXISTS (
    SELECT 1
    FROM service_identities si
    JOIN service_credentials sc
      ON sc.service_code = si.service_code
    JOIN service_permission_grants spg
      ON spg.caller_service_code = si.service_code
    JOIN permissions p
      ON p.code = spg.permission_code
    WHERE si.service_code = $1
      AND si.enabled
      AND sc.enabled
      AND sc.token_hash = $4
      AND sc.revoked_at IS NULL
      AND (sc.expires_at IS NULL OR sc.expires_at > NOW())
      AND spg.enabled
      AND spg.permission_code = $2
      AND ($3 = '' OR spg.api_id = $3)
)
`, serviceCode, permissionCode, apiID, tokenHash).Scan(&ok)
	if err != nil {
		return false, err
	}
	action := "service.permission_check.deny"
	if ok {
		action = "service.permission_check.allow"
		if _, err := r.db.Exec(ctx, `
UPDATE service_credentials
SET last_used_at = NOW(), updated_at = NOW()
WHERE service_code = $1
  AND token_hash = $2
`, serviceCode, tokenHash); err != nil {
			return false, err
		}
	}
	if err := r.writeServiceAudit(ctx, action, serviceCode, permissionCode, apiID, map[string]any{
		"allowed": ok,
	}); err != nil {
		return false, err
	}
	return ok, nil
}

func (r *AdminRepository) ListServiceIdentity(ctx context.Context, serviceCode string) (ServiceIdentityDetails, error) {
	serviceCode = strings.TrimSpace(serviceCode)
	if serviceCode == "" {
		return ServiceIdentityDetails{}, errors.New("service_code is required")
	}
	var details ServiceIdentityDetails
	err := r.db.QueryRow(ctx, `
SELECT service_code, enabled
FROM service_identities
WHERE service_code = $1
`, serviceCode).Scan(&details.ServiceCode, &details.Enabled)
	if err != nil {
		return ServiceIdentityDetails{}, err
	}
	credentials, err := r.ListServiceCredentials(ctx, serviceCode)
	if err != nil {
		return ServiceIdentityDetails{}, err
	}
	grants, err := r.ListServiceGrants(ctx, serviceCode)
	if err != nil {
		return ServiceIdentityDetails{}, err
	}
	details.Credentials = credentials
	details.Grants = grants
	return details, nil
}

func (r *AdminRepository) ListServiceCredentials(ctx context.Context, serviceCode string) ([]ServiceCredentialListItem, error) {
	serviceCode = strings.TrimSpace(serviceCode)
	if serviceCode == "" {
		return nil, errors.New("service_code is required")
	}
	rows, err := r.db.Query(ctx, `
SELECT
    service_code,
    token_hint,
    enabled,
    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    COALESCE(to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'), ''),
    COALESCE(to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'), ''),
    COALESCE(to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'), '')
FROM service_credentials
WHERE service_code = $1
ORDER BY created_at DESC
`, serviceCode)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]ServiceCredentialListItem, 0)
	for rows.Next() {
		var item ServiceCredentialListItem
		if err := rows.Scan(&item.ServiceCode, &item.TokenHint, &item.Enabled, &item.CreatedAt, &item.UpdatedAt, &item.ExpiresAt, &item.RevokedAt, &item.LastUsedAt); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (r *AdminRepository) ListServiceGrants(ctx context.Context, serviceCode string) ([]ServiceGrantListItem, error) {
	serviceCode = strings.TrimSpace(serviceCode)
	if serviceCode == "" {
		return nil, errors.New("service_code is required")
	}
	rows, err := r.db.Query(ctx, `
SELECT api_id, permission_code, provider_service_code, enabled
FROM service_permission_grants
WHERE caller_service_code = $1
ORDER BY api_id, permission_code
`, serviceCode)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]ServiceGrantListItem, 0)
	for rows.Next() {
		var item ServiceGrantListItem
		if err := rows.Scan(&item.APIID, &item.PermissionCode, &item.ProviderServiceCode, &item.Enabled); err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, rows.Err()
}

func (r *AdminRepository) AddServiceCredential(ctx context.Context, actorID int64, serviceCode string, input ServiceCredentialInput) (ServiceCredentialListItem, error) {
	serviceCode = strings.TrimSpace(serviceCode)
	tokenHash := serviceCredentialTokenHash(input.Token)
	if serviceCode == "" {
		return ServiceCredentialListItem{}, errors.New("service_code is required")
	}
	if tokenHash == "" {
		return ServiceCredentialListItem{}, errors.New("credential token is required")
	}
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return ServiceCredentialListItem{}, err
	}
	defer func() {
		_ = tx.Rollback(ctx)
	}()
	if _, err := tx.Exec(ctx, `
INSERT INTO service_identities(service_code, enabled, updated_at)
VALUES($1, TRUE, NOW())
ON CONFLICT(service_code)
DO UPDATE SET enabled = TRUE, updated_at = NOW()
`, serviceCode); err != nil {
		return ServiceCredentialListItem{}, err
	}
	if _, err := tx.Exec(ctx, `
INSERT INTO service_credentials(service_code, token_hash, token_hint, enabled, expires_at, revoked_at, updated_at)
VALUES($1, $2, $3, TRUE, $4, NULL, NOW())
ON CONFLICT(service_code, token_hash)
DO UPDATE SET token_hint = EXCLUDED.token_hint,
              enabled = TRUE,
              expires_at = EXCLUDED.expires_at,
              revoked_at = NULL,
              updated_at = NOW()
`, serviceCode, tokenHash, serviceCredentialTokenHint(input.Token), input.ExpiresAt); err != nil {
		return ServiceCredentialListItem{}, err
	}
	if err := writeAuditTx(ctx, tx, actorID, "service.credential.create", serviceCode, "", "", "", map[string]any{
		"token_hint": serviceCredentialTokenHint(input.Token),
		"expires_at": input.ExpiresAt,
	}); err != nil {
		return ServiceCredentialListItem{}, err
	}
	if err := tx.Commit(ctx); err != nil {
		return ServiceCredentialListItem{}, err
	}
	return r.getServiceCredentialByHash(ctx, serviceCode, tokenHash)
}

func (r *AdminRepository) RevokeServiceCredential(ctx context.Context, actorID int64, serviceCode string, token string, tokenHash string, reason string) error {
	serviceCode = strings.TrimSpace(serviceCode)
	tokenHash = strings.TrimSpace(tokenHash)
	if tokenHash == "" {
		tokenHash = serviceCredentialTokenHash(token)
	}
	if serviceCode == "" {
		return errors.New("service_code is required")
	}
	if tokenHash == "" {
		return errors.New("credential token or token_hash is required")
	}
	tx, err := r.db.Begin(ctx)
	if err != nil {
		return err
	}
	defer func() {
		_ = tx.Rollback(ctx)
	}()
	tag, err := tx.Exec(ctx, `
UPDATE service_credentials
SET enabled = FALSE,
    revoked_at = COALESCE(revoked_at, NOW()),
    updated_at = NOW()
WHERE service_code = $1
  AND token_hash = $2
`, serviceCode, tokenHash)
	if err != nil {
		return err
	}
	if err := writeAuditTx(ctx, tx, actorID, "service.credential.revoke", serviceCode, "", "", "", map[string]any{
		"rows":   tag.RowsAffected(),
		"reason": strings.TrimSpace(reason),
	}); err != nil {
		return err
	}
	return tx.Commit(ctx)
}

func (r *AdminRepository) getServiceCredentialByHash(ctx context.Context, serviceCode string, tokenHash string) (ServiceCredentialListItem, error) {
	var item ServiceCredentialListItem
	err := r.db.QueryRow(ctx, `
SELECT
    service_code,
    token_hint,
    enabled,
    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
    COALESCE(to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'), ''),
    COALESCE(to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'), ''),
    COALESCE(to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'), '')
FROM service_credentials
WHERE service_code = $1
  AND token_hash = $2
`, serviceCode, tokenHash).Scan(&item.ServiceCode, &item.TokenHint, &item.Enabled, &item.CreatedAt, &item.UpdatedAt, &item.ExpiresAt, &item.RevokedAt, &item.LastUsedAt)
	return item, err
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
	if strings.TrimSpace(identity.CredentialToken) != "" {
		return false
	}
	return true
}

func (r *AdminRepository) registerServiceIdentity(ctx context.Context, tx pgx.Tx, actorID int64, serviceCode string, identity *ServiceIdentityInput) error {
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
	if tokenHash := serviceCredentialTokenHash(identity.CredentialToken); tokenHash != "" {
		if _, err := tx.Exec(ctx, `
INSERT INTO service_credentials(service_code, token_hash, token_hint, enabled, expires_at, revoked_at, updated_at)
VALUES($1, $2, $3, TRUE, $4, NULL, NOW())
ON CONFLICT(service_code, token_hash)
DO UPDATE SET token_hint = EXCLUDED.token_hint,
              enabled = TRUE,
              expires_at = EXCLUDED.expires_at,
              revoked_at = NULL,
              updated_at = NOW()
`, serviceCode, tokenHash, serviceCredentialTokenHint(identity.CredentialToken), identity.CredentialExpiresAt); err != nil {
			return err
		}
		if err := writeAuditTx(ctx, tx, actorID, "service.credential.upsert", serviceCode, "", "", "", map[string]any{
			"token_hint": serviceCredentialTokenHint(identity.CredentialToken),
			"expires_at": identity.CredentialExpiresAt,
		}); err != nil {
			return err
		}
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
		if err := writeAuditTx(ctx, tx, actorID, "service.grant.upsert", serviceCode, permissionCode, apiID, providerService, nil); err != nil {
			return err
		}
	}
	return nil
}

func (r *AdminRepository) writeServiceAudit(ctx context.Context, action string, serviceCode string, permissionCode string, apiID string, metadata map[string]any) error {
	if metadata == nil {
		metadata = map[string]any{}
	}
	if strings.TrimSpace(apiID) != "" {
		metadata["api_id"] = strings.TrimSpace(apiID)
	}
	return permission.WriteAuditLog(
		ctx,
		r.db,
		permission.Principal{Type: permission.PrincipalService, ID: 0},
		action,
		permission.Principal{Type: permission.PrincipalService, ID: 0},
		permissionCode,
		0,
		"",
		permission.SystemScope(),
		"",
		withServiceMetadata(serviceCode, metadata),
	)
}

func serviceCredentialTokenHash(token string) string {
	token = strings.TrimSpace(token)
	if token == "" {
		return ""
	}
	sum := sha256.Sum256([]byte(token))
	return "sha256:" + hex.EncodeToString(sum[:])
}

func serviceCredentialTokenHint(token string) string {
	token = strings.TrimSpace(token)
	if token == "" {
		return ""
	}
	if len(token) <= 8 {
		return token
	}
	return token[:4] + "..." + token[len(token)-4:]
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

func writeAuditTx(ctx context.Context, tx pgx.Tx, actorID int64, action string, serviceCode string, permissionCode string, apiID string, providerService string, metadata map[string]any) error {
	if metadata == nil {
		metadata = map[string]any{}
	}
	if strings.TrimSpace(apiID) != "" {
		metadata["api_id"] = strings.TrimSpace(apiID)
	}
	if strings.TrimSpace(providerService) != "" {
		metadata["provider_service"] = strings.TrimSpace(providerService)
	}
	metadata = withServiceMetadata(serviceCode, metadata)
	_, err := tx.Exec(ctx, `
INSERT INTO permission_audit_logs(
    actor_type,
    actor_id,
    action,
    target_type,
    target_id,
    permission_code,
    scope_type,
    scope_id,
    metadata
)
VALUES('user', $1, $2, 'service', 0, $3, 'system', 0, $4)
`, actorID, strings.TrimSpace(action), strings.TrimSpace(permissionCode), metadata)
	return err
}

func withServiceMetadata(serviceCode string, metadata map[string]any) map[string]any {
	if metadata == nil {
		metadata = map[string]any{}
	}
	if serviceCode = strings.TrimSpace(serviceCode); serviceCode != "" {
		metadata["service_code"] = serviceCode
	}
	return metadata
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
