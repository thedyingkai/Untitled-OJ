package permission

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
)

const (
	PrincipalUser    = "user"
	PrincipalTeam    = "team"
	PrincipalGroup   = "group"
	PrincipalService = "service"

	ScopeSystem = "system"

	EffectAllow = "allow"
	EffectDeny  = "deny"

	RoleSuperAdmin = "super_admin"
)

var (
	ErrForbidden = errors.New("forbidden")
)

type Principal struct {
	Type string
	ID   int64
}

type Scope struct {
	Type string
	ID   int64
}

func UserPrincipal(userID int64) Principal {
	return Principal{
		Type: PrincipalUser,
		ID:   userID,
	}
}

func SystemScope() Scope {
	return Scope{
		Type: ScopeSystem,
		ID:   0,
	}
}

func HasUserPermission(
	ctx context.Context,
	db *pgxpool.Pool,
	userID int64,
	permissionCode string,
	scope Scope,
) (bool, error) {
	return HasPermission(ctx, db, UserPrincipal(userID), permissionCode, scope)
}

func RequireUserPermission(
	ctx context.Context,
	db *pgxpool.Pool,
	userID int64,
	permissionCode string,
	scope Scope,
) error {
	ok, err := HasUserPermission(ctx, db, userID, permissionCode, scope)
	if err != nil {
		return err
	}
	if !ok {
		return ErrForbidden
	}
	return nil
}

func HasPermission(
	ctx context.Context,
	db *pgxpool.Pool,
	principal Principal,
	permissionCode string,
	scope Scope,
) (bool, error) {
	principal = normalizePrincipal(principal)
	scope = normalizeScope(scope)
	permissionCode = strings.TrimSpace(permissionCode)

	if principal.Type == "" || principal.ID <= 0 {
		return false, nil
	}
	if permissionCode == "" {
		return false, nil
	}

	if principal.Type == PrincipalUser {
		super, err := hasSuperAdmin(ctx, db, principal.ID)
		if err != nil {
			return false, err
		}
		if super {
			return true, nil
		}
	}

	scopes, err := collectScopes(ctx, db, scope)
	if err != nil {
		return false, err
	}

	for _, s := range scopes {
		deny, err := hasDirectAssignment(ctx, db, principal, permissionCode, s, EffectDeny)
		if err != nil {
			return false, err
		}
		if deny {
			return false, nil
		}
	}

	for _, s := range scopes {
		allow, err := hasDirectAssignment(ctx, db, principal, permissionCode, s, EffectAllow)
		if err != nil {
			return false, err
		}
		if allow {
			return true, nil
		}
	}

	if principal.Type == PrincipalUser {
		ok, err := hasGlobalUserRolePermission(ctx, db, principal.ID, permissionCode)
		if err != nil {
			return false, err
		}
		if ok {
			return true, nil
		}
	}

	for _, s := range scopes {
		ok, err := hasScopedRolePermission(ctx, db, principal, permissionCode, s)
		if err != nil {
			return false, err
		}
		if ok {
			return true, nil
		}
	}

	return false, nil
}

func BindRole(
	ctx context.Context,
	db *pgxpool.Pool,
	actor Principal,
	target Principal,
	roleName string,
	scope Scope,
	expiresAt *time.Time,
) error {
	actor = normalizeActor(actor)
	target = normalizePrincipal(target)
	scope = normalizeScope(scope)
	roleName = strings.TrimSpace(roleName)

	if target.Type == "" || target.ID <= 0 {
		return fmt.Errorf("invalid target principal")
	}
	if roleName == "" {
		return fmt.Errorf("role name is empty")
	}

	roleID, err := getRoleIDByName(ctx, db, roleName)
	if err != nil {
		return err
	}

	_, err = db.Exec(
		ctx,
		`
		INSERT INTO role_bindings(
			principal_type,
			principal_id,
			role_id,
			scope_type,
			scope_id,
			granted_by_type,
			granted_by_id,
			expires_at
		)
		VALUES($1,$2,$3,$4,$5,$6,$7,$8)
		ON CONFLICT(principal_type, principal_id, role_id, scope_type, scope_id)
		DO UPDATE SET
			granted_by_type = EXCLUDED.granted_by_type,
			granted_by_id = EXCLUDED.granted_by_id,
			expires_at = EXCLUDED.expires_at
		`,
		target.Type,
		target.ID,
		roleID,
		scope.Type,
		scope.ID,
		actor.Type,
		actor.ID,
		expiresAt,
	)
	if err != nil {
		return err
	}

	return writeAuditLog(ctx, db, actor, "role.bind", target, "", roleID, roleName, scope, "", nil)
}

func UnbindRole(
	ctx context.Context,
	db *pgxpool.Pool,
	actor Principal,
	target Principal,
	roleName string,
	scope Scope,
) error {
	actor = normalizeActor(actor)
	target = normalizePrincipal(target)
	scope = normalizeScope(scope)
	roleName = strings.TrimSpace(roleName)

	if target.Type == "" || target.ID <= 0 {
		return fmt.Errorf("invalid target principal")
	}
	if roleName == "" {
		return fmt.Errorf("role name is empty")
	}

	roleID, err := getRoleIDByName(ctx, db, roleName)
	if err != nil {
		return err
	}

	_, err = db.Exec(
		ctx,
		`
		DELETE FROM role_bindings
		WHERE principal_type = $1
		  AND principal_id = $2
		  AND role_id = $3
		  AND scope_type = $4
		  AND scope_id = $5
		`,
		target.Type,
		target.ID,
		roleID,
		scope.Type,
		scope.ID,
	)
	if err != nil {
		return err
	}

	return writeAuditLog(ctx, db, actor, "role.unbind", target, "", roleID, roleName, scope, "", nil)
}

func AssignPermission(
	ctx context.Context,
	db *pgxpool.Pool,
	actor Principal,
	target Principal,
	permissionCode string,
	scope Scope,
	effect string,
	reason string,
	expiresAt *time.Time,
) error {
	actor = normalizeActor(actor)
	target = normalizePrincipal(target)
	scope = normalizeScope(scope)
	permissionCode = strings.TrimSpace(permissionCode)
	effect = strings.ToLower(strings.TrimSpace(effect))

	if target.Type == "" || target.ID <= 0 {
		return fmt.Errorf("invalid target principal")
	}
	if permissionCode == "" {
		return fmt.Errorf("permission code is empty")
	}
	if effect != EffectAllow && effect != EffectDeny {
		return fmt.Errorf("invalid permission effect: %s", effect)
	}

	_, err := db.Exec(
		ctx,
		`
		INSERT INTO permission_assignments(
			principal_type,
			principal_id,
			permission_code,
			scope_type,
			scope_id,
			effect,
			granted_by_type,
			granted_by_id,
			reason,
			expires_at
		)
		VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
		ON CONFLICT(principal_type, principal_id, permission_code, scope_type, scope_id)
		DO UPDATE SET
			effect = EXCLUDED.effect,
			granted_by_type = EXCLUDED.granted_by_type,
			granted_by_id = EXCLUDED.granted_by_id,
			reason = EXCLUDED.reason,
			expires_at = EXCLUDED.expires_at
		`,
		target.Type,
		target.ID,
		permissionCode,
		scope.Type,
		scope.ID,
		effect,
		actor.Type,
		actor.ID,
		reason,
		expiresAt,
	)
	if err != nil {
		return err
	}

	return writeAuditLog(ctx, db, actor, "permission.assign", target, permissionCode, 0, "", scope, effect, map[string]any{
		"reason": reason,
	})
}

func RevokePermissionAssignment(
	ctx context.Context,
	db *pgxpool.Pool,
	actor Principal,
	target Principal,
	permissionCode string,
	scope Scope,
) error {
	actor = normalizeActor(actor)
	target = normalizePrincipal(target)
	scope = normalizeScope(scope)
	permissionCode = strings.TrimSpace(permissionCode)

	if target.Type == "" || target.ID <= 0 {
		return fmt.Errorf("invalid target principal")
	}
	if permissionCode == "" {
		return fmt.Errorf("permission code is empty")
	}

	_, err := db.Exec(
		ctx,
		`
		DELETE FROM permission_assignments
		WHERE principal_type = $1
		  AND principal_id = $2
		  AND permission_code = $3
		  AND scope_type = $4
		  AND scope_id = $5
		`,
		target.Type,
		target.ID,
		permissionCode,
		scope.Type,
		scope.ID,
	)
	if err != nil {
		return err
	}

	return writeAuditLog(ctx, db, actor, "permission.revoke", target, permissionCode, 0, "", scope, "", nil)
}

func AddResourceEdge(
	ctx context.Context,
	db *pgxpool.Pool,
	parent Scope,
	child Scope,
	relation string,
) error {
	parent = normalizeScope(parent)
	child = normalizeScope(child)
	relation = strings.TrimSpace(relation)
	if relation == "" {
		relation = "contains"
	}

	if parent.Type == "" || child.Type == "" {
		return fmt.Errorf("invalid resource edge")
	}
	if parent.Type == child.Type && parent.ID == child.ID {
		return fmt.Errorf("resource edge cannot point to itself")
	}

	_, err := db.Exec(
		ctx,
		`
		INSERT INTO resource_edges(parent_type, parent_id, child_type, child_id, relation)
		VALUES($1,$2,$3,$4,$5)
		ON CONFLICT(parent_type, parent_id, child_type, child_id, relation)
		DO NOTHING
		`,
		parent.Type,
		parent.ID,
		child.Type,
		child.ID,
		relation,
	)
	return err
}

func RemoveResourceEdge(
	ctx context.Context,
	db *pgxpool.Pool,
	parent Scope,
	child Scope,
	relation string,
) error {
	parent = normalizeScope(parent)
	child = normalizeScope(child)
	relation = strings.TrimSpace(relation)
	if relation == "" {
		relation = "contains"
	}

	if parent.Type == "" || child.Type == "" {
		return fmt.Errorf("invalid resource edge")
	}

	_, err := db.Exec(
		ctx,
		`
		DELETE FROM resource_edges
		WHERE parent_type = $1
		  AND parent_id = $2
		  AND child_type = $3
		  AND child_id = $4
		  AND relation = $5
		`,
		parent.Type,
		parent.ID,
		child.Type,
		child.ID,
		relation,
	)
	return err
}

func RegisterResourceType(
	ctx context.Context,
	db *pgxpool.Pool,
	code string,
	ServiceCode string,
	name string,
	description string,
) error {
	code = strings.TrimSpace(code)
	ServiceCode = defaultString(ServiceCode, "core")
	name = strings.TrimSpace(name)
	description = strings.TrimSpace(description)

	if code == "" {
		return fmt.Errorf("resource type code is empty")
	}
	if name == "" {
		name = code
	}

	_, err := db.Exec(
		ctx,
		`
		INSERT INTO resource_types(code, service_code, name, description)
		VALUES($1,$2,$3,$4)
		ON CONFLICT(code)
		DO UPDATE SET
			service_code = EXCLUDED.service_code,
			name = EXCLUDED.name,
			description = EXCLUDED.description
		`,
		code,
		ServiceCode,
		name,
		description,
	)

	return err
}

func RegisterPermission(
	ctx context.Context,
	db *pgxpool.Pool,
	code string,
	ServiceCode string,
	name string,
	description string,
) error {
	code = strings.TrimSpace(code)
	ServiceCode = defaultString(ServiceCode, "core")
	name = strings.TrimSpace(name)
	description = strings.TrimSpace(description)

	if code == "" {
		return fmt.Errorf("permission code is empty")
	}
	if name == "" {
		name = code
	}

	_, err := db.Exec(
		ctx,
		`
		INSERT INTO permissions(code, service_code, name, description)
		VALUES($1,$2,$3,$4)
		ON CONFLICT(code)
		DO UPDATE SET
			service_code = EXCLUDED.service_code,
			name = EXCLUDED.name,
			description = EXCLUDED.description
		`,
		code,
		ServiceCode,
		name,
		description,
	)

	return err
}

func GrantRolePermission(
	ctx context.Context,
	db *pgxpool.Pool,
	roleName string,
	permissionCode string,
) error {
	roleName = strings.TrimSpace(roleName)
	permissionCode = strings.TrimSpace(permissionCode)

	if roleName == "" {
		return fmt.Errorf("role name is empty")
	}
	if permissionCode == "" {
		return fmt.Errorf("permission code is empty")
	}

	roleID, err := getRoleIDByName(ctx, db, roleName)
	if err != nil {
		return err
	}

	_, err = db.Exec(
		ctx,
		`
		INSERT INTO role_permissions(role_id, permission_code)
		VALUES($1,$2)
		ON CONFLICT(role_id, permission_code)
		DO NOTHING
		`,
		roleID,
		permissionCode,
	)

	return err
}

func RevokeRolePermission(
	ctx context.Context,
	db *pgxpool.Pool,
	roleName string,
	permissionCode string,
) error {
	roleName = strings.TrimSpace(roleName)
	permissionCode = strings.TrimSpace(permissionCode)

	if roleName == "" {
		return fmt.Errorf("role name is empty")
	}
	if permissionCode == "" {
		return fmt.Errorf("permission code is empty")
	}

	roleID, err := getRoleIDByName(ctx, db, roleName)
	if err != nil {
		return err
	}

	_, err = db.Exec(
		ctx,
		`
		DELETE FROM role_permissions
		WHERE role_id = $1
		  AND permission_code = $2
		`,
		roleID,
		permissionCode,
	)

	return err
}

func collectScopes(ctx context.Context, db *pgxpool.Pool, scope Scope) ([]Scope, error) {
	scope = normalizeScope(scope)

	rows, err := db.Query(
		ctx,
		`
		WITH RECURSIVE ancestors(scope_type, scope_id, depth) AS (
			SELECT $1::text, $2::bigint, 0

			UNION ALL

			SELECT re.parent_type, re.parent_id, ancestors.depth + 1
			FROM resource_edges re
			JOIN ancestors
			  ON re.child_type = ancestors.scope_type
			 AND re.child_id = ancestors.scope_id
			WHERE ancestors.depth < 16
		)
		SELECT DISTINCT scope_type, scope_id
		FROM ancestors
		`,
		scope.Type,
		scope.ID,
	)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	result := make([]Scope, 0)
	seen := make(map[string]bool)

	add := func(s Scope) {
		s = normalizeScope(s)
		if s.Type == "" {
			return
		}
		key := fmt.Sprintf("%s:%d", s.Type, s.ID)
		if seen[key] {
			return
		}
		seen[key] = true
		result = append(result, s)
	}

	for rows.Next() {
		var s Scope
		if err := rows.Scan(&s.Type, &s.ID); err != nil {
			return nil, err
		}

		add(s)
		add(Scope{Type: s.Type, ID: 0})
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	add(SystemScope())

	return result, nil
}

func hasSuperAdmin(ctx context.Context, db *pgxpool.Pool, userID int64) (bool, error) {
	var ok bool

	err := db.QueryRow(
		ctx,
		`
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
		)
		`,
		userID,
	).Scan(&ok)

	return ok, err
}

func hasDirectAssignment(
	ctx context.Context,
	db *pgxpool.Pool,
	principal Principal,
	permissionCode string,
	scope Scope,
	effect string,
) (bool, error) {
	var ok bool

	err := db.QueryRow(
		ctx,
		`
		SELECT EXISTS (
			SELECT 1
			FROM permission_assignments
			WHERE principal_type = $1
			  AND principal_id = $2
			  AND permission_code = $3
			  AND scope_type = $4
			  AND scope_id = $5
			  AND effect = $6
			  AND (expires_at IS NULL OR expires_at > NOW())
		)
		`,
		principal.Type,
		principal.ID,
		permissionCode,
		scope.Type,
		scope.ID,
		effect,
	).Scan(&ok)

	return ok, err
}

func hasGlobalUserRolePermission(
	ctx context.Context,
	db *pgxpool.Pool,
	userID int64,
	permissionCode string,
) (bool, error) {
	var ok bool

	err := db.QueryRow(
		ctx,
		`
		SELECT EXISTS (
			SELECT 1
			FROM user_roles ur
			JOIN role_permissions rp ON rp.role_id = ur.role_id
			WHERE ur.user_id = $1
			  AND rp.permission_code = $2
		)
		`,
		userID,
		permissionCode,
	).Scan(&ok)

	return ok, err
}

func hasScopedRolePermission(
	ctx context.Context,
	db *pgxpool.Pool,
	principal Principal,
	permissionCode string,
	scope Scope,
) (bool, error) {
	var ok bool

	err := db.QueryRow(
		ctx,
		`
		SELECT EXISTS (
			SELECT 1
			FROM role_bindings rb
			JOIN role_permissions rp ON rp.role_id = rb.role_id
			WHERE rb.principal_type = $1
			  AND rb.principal_id = $2
			  AND rb.scope_type = $3
			  AND rb.scope_id = $4
			  AND rp.permission_code = $5
			  AND (rb.expires_at IS NULL OR rb.expires_at > NOW())
		)
		`,
		principal.Type,
		principal.ID,
		scope.Type,
		scope.ID,
		permissionCode,
	).Scan(&ok)

	return ok, err
}

func getRoleIDByName(ctx context.Context, db *pgxpool.Pool, roleName string) (int64, error) {
	var roleID int64

	err := db.QueryRow(
		ctx,
		`
		SELECT id
		FROM roles
		WHERE name = $1
		`,
		roleName,
	).Scan(&roleID)
	if err != nil {
		return 0, err
	}

	return roleID, nil
}

func writeAuditLog(
	ctx context.Context,
	db *pgxpool.Pool,
	actor Principal,
	action string,
	target Principal,
	permissionCode string,
	roleID int64,
	roleName string,
	scope Scope,
	effect string,
	metadata map[string]any,
) error {
	if metadata == nil {
		metadata = map[string]any{}
	}

	_, err := db.Exec(
		ctx,
		`
		INSERT INTO permission_audit_logs(
			actor_type,
			actor_id,
			action,
			target_type,
			target_id,
			permission_code,
			role_id,
			role_name,
			scope_type,
			scope_id,
			effect,
			metadata
		)
		VALUES($1,$2,$3,$4,$5,$6,NULLIF($7,0),$8,$9,$10,$11,$12)
		`,
		actor.Type,
		actor.ID,
		action,
		target.Type,
		target.ID,
		permissionCode,
		roleID,
		roleName,
		scope.Type,
		scope.ID,
		effect,
		metadata,
	)

	return err
}

func WriteAuditLog(
	ctx context.Context,
	db *pgxpool.Pool,
	actor Principal,
	action string,
	target Principal,
	permissionCode string,
	roleID int64,
	roleName string,
	scope Scope,
	effect string,
	metadata map[string]any,
) error {
	return writeAuditLog(ctx, db, normalizeActor(actor), strings.TrimSpace(action), normalizePrincipal(target), strings.TrimSpace(permissionCode), roleID, strings.TrimSpace(roleName), normalizeScope(scope), strings.TrimSpace(effect), metadata)
}

func normalizePrincipal(p Principal) Principal {
	p.Type = strings.ToLower(strings.TrimSpace(p.Type))
	if p.Type == "" {
		p.Type = PrincipalUser
	}
	return p
}

func normalizeActor(p Principal) Principal {
	p.Type = strings.ToLower(strings.TrimSpace(p.Type))
	if p.Type == "" || p.ID <= 0 {
		return Principal{
			Type: "system",
			ID:   0,
		}
	}
	return p
}

func normalizeScope(s Scope) Scope {
	s.Type = strings.ToLower(strings.TrimSpace(s.Type))
	if s.Type == "" {
		s.Type = ScopeSystem
	}
	if s.ID < 0 {
		s.ID = 0
	}
	return s
}

func defaultString(value string, fallback string) string {
	value = strings.TrimSpace(value)
	if value == "" {
		return fallback
	}
	return value
}
