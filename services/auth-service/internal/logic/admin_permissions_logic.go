package logic

import (
	"context"
	"errors"
	"strings"
	"time"

	"ojos-auth-service/internal/svc"
	"ojos-auth-service/internal/types"
	"ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminPermissionsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminPermissionsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminPermissionsLogic {
	return &AdminPermissionsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminPermissionsLogic) ListUsers() (*types.ListUsersResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	users, err := l.svcCtx.AdminRepo.ListUsers(l.ctx)
	if err != nil {
		return nil, err
	}
	items := make([]types.UserItem, 0, len(users))
	for _, user := range users {
		items = append(items, types.UserItem{
			UserId:    user.UserID,
			Username:  user.Username,
			Email:     user.Email,
			Roles:     user.Roles,
			CreatedAt: user.CreatedAt.UTC().Format(time.RFC3339Nano),
		})
	}
	return &types.ListUsersResp{Code: 0, Msg: "success", Data: items}, nil
}

func (l *AdminPermissionsLogic) ListRoles() (*types.ListRolesResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	roles, err := l.svcCtx.AdminRepo.ListRoles(l.ctx)
	if err != nil {
		return nil, err
	}
	items := make([]types.RoleItem, 0, len(roles))
	for _, role := range roles {
		items = append(items, types.RoleItem{
			Id:          role.ID,
			Name:        role.Name,
			ServiceCode: role.ServiceCode,
			Description: role.Description,
			IsSystem:    role.IsSystem,
		})
	}
	return &types.ListRolesResp{Code: 0, Msg: "success", Data: items}, nil
}

func (l *AdminPermissionsLogic) ListPermissions() (*types.ListPermissionsResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	if l.svcCtx.SmokeAuth != nil {
		perms := l.svcCtx.SmokeAuth.ListPermissions()
		items := make([]types.PermissionItem, 0, len(perms))
		for _, perm := range perms {
			items = append(items, types.PermissionItem{
				Code:        perm.Code,
				ServiceCode: perm.ServiceCode,
				Name:        perm.Name,
				Description: perm.Description,
			})
		}
		return &types.ListPermissionsResp{Code: 0, Msg: "success", Data: items}, nil
	}
	perms, err := l.svcCtx.AdminRepo.ListPermissions(l.ctx)
	if err != nil {
		return nil, err
	}
	items := make([]types.PermissionItem, 0, len(perms))
	for _, perm := range perms {
		items = append(items, types.PermissionItem{
			Code:        perm.Code,
			ServiceCode: perm.ServiceCode,
			Name:        perm.Name,
			Description: perm.Description,
		})
	}
	return &types.ListPermissionsResp{Code: 0, Msg: "success", Data: items}, nil
}

func (l *AdminPermissionsLogic) ListResourceTypes() (*types.ListResourceTypesResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	items, err := l.svcCtx.AdminRepo.ListResourceTypes(l.ctx)
	if err != nil {
		return nil, err
	}
	out := make([]types.ResourceTypeItem, 0, len(items))
	for _, item := range items {
		out = append(out, types.ResourceTypeItem{
			Code:        item.Code,
			ServiceCode: item.ServiceCode,
			Name:        item.Name,
			Description: item.Description,
			CreatedAt:   item.CreatedAt,
		})
	}
	return &types.ListResourceTypesResp{Code: 0, Msg: "success", Data: out}, nil
}

func (l *AdminPermissionsLogic) ListRoleBindings() (*types.ListRoleBindingsResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	items, err := l.svcCtx.AdminRepo.ListRoleBindings(l.ctx)
	if err != nil {
		return nil, err
	}
	out := make([]types.RoleBindingItem, 0, len(items))
	for _, item := range items {
		out = append(out, types.RoleBindingItem{
			Id:            item.ID,
			PrincipalType: item.PrincipalType,
			PrincipalId:   item.PrincipalID,
			Role:          item.Role,
			ScopeType:     item.ScopeType,
			ScopeId:       item.ScopeID,
			GrantedByType: item.GrantedByType,
			GrantedById:   item.GrantedByID,
			ExpiresAt:     item.ExpiresAt,
			CreatedAt:     item.CreatedAt,
		})
	}
	return &types.ListRoleBindingsResp{Code: 0, Msg: "success", Data: out}, nil
}

func (l *AdminPermissionsLogic) ListPermissionAssignments() (*types.ListPermissionAssignmentsResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	items, err := l.svcCtx.AdminRepo.ListPermissionAssignments(l.ctx)
	if err != nil {
		return nil, err
	}
	out := make([]types.PermissionAssignmentItem, 0, len(items))
	for _, item := range items {
		out = append(out, types.PermissionAssignmentItem{
			Id:             item.ID,
			PrincipalType:  item.PrincipalType,
			PrincipalId:    item.PrincipalID,
			PermissionCode: item.PermissionCode,
			ScopeType:      item.ScopeType,
			ScopeId:        item.ScopeID,
			Effect:         item.Effect,
			Reason:         item.Reason,
			GrantedByType:  item.GrantedByType,
			GrantedById:    item.GrantedByID,
			ExpiresAt:      item.ExpiresAt,
			CreatedAt:      item.CreatedAt,
		})
	}
	return &types.ListPermissionAssignmentsResp{Code: 0, Msg: "success", Data: out}, nil
}

func (l *AdminPermissionsLogic) ListResourceEdges() (*types.ListResourceEdgesResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	items, err := l.svcCtx.AdminRepo.ListResourceEdges(l.ctx)
	if err != nil {
		return nil, err
	}
	out := make([]types.ResourceEdgeItem, 0, len(items))
	for _, item := range items {
		out = append(out, types.ResourceEdgeItem{
			Id:         item.ID,
			ParentType: item.ParentType,
			ParentId:   item.ParentID,
			ChildType:  item.ChildType,
			ChildId:    item.ChildID,
			Relation:   item.Relation,
			CreatedAt:  item.CreatedAt,
		})
	}
	return &types.ListResourceEdgesResp{Code: 0, Msg: "success", Data: out}, nil
}

func (l *AdminPermissionsLogic) UpsertRole(req *types.RoleManageReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req == nil || strings.TrimSpace(req.Name) == "" {
		return nil, errors.New("role name is required")
	}
	if err := l.svcCtx.AdminRepo.UpsertRole(l.ctx, actorID, req.Name, req.ServiceCode, req.Description, req.IsSystem); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) DeleteRole(req *types.DeleteRoleReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req == nil || strings.TrimSpace(req.Name) == "" {
		return nil, errors.New("role name is required")
	}
	if err := l.svcCtx.AdminRepo.DeleteRole(l.ctx, actorID, req.Name); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) GrantRolePermission(req *types.RolePermissionReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req == nil || strings.TrimSpace(req.Role) == "" || strings.TrimSpace(req.Permission) == "" {
		return nil, errors.New("role and permission are required")
	}
	if err := permission.GrantRolePermission(l.ctx, l.svcCtx.DB, req.Role, req.Permission); err != nil {
		return nil, err
	}
	if err := permission.WriteAuditLog(l.ctx, l.svcCtx.DB, permission.UserPrincipal(actorID), "role_permission.grant", permission.Principal{Type: "role", ID: 0}, strings.TrimSpace(req.Permission), 0, strings.TrimSpace(req.Role), permission.SystemScope(), "", map[string]any{
		"role":       strings.TrimSpace(req.Role),
		"permission": strings.TrimSpace(req.Permission),
	}); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) RevokeRolePermission(req *types.RolePermissionReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req == nil || strings.TrimSpace(req.Role) == "" || strings.TrimSpace(req.Permission) == "" {
		return nil, errors.New("role and permission are required")
	}
	if err := permission.RevokeRolePermission(l.ctx, l.svcCtx.DB, req.Role, req.Permission); err != nil {
		return nil, err
	}
	if err := permission.WriteAuditLog(l.ctx, l.svcCtx.DB, permission.UserPrincipal(actorID), "role_permission.revoke", permission.Principal{Type: "role", ID: 0}, strings.TrimSpace(req.Permission), 0, strings.TrimSpace(req.Role), permission.SystemScope(), "", map[string]any{
		"role":       strings.TrimSpace(req.Role),
		"permission": strings.TrimSpace(req.Permission),
	}); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) UpsertPermission(req *types.PermissionManageReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req == nil || strings.TrimSpace(req.Code) == "" {
		return nil, errors.New("permission code is required")
	}
	if err := permission.RegisterPermission(l.ctx, l.svcCtx.DB, req.Code, req.ServiceCode, req.Name, req.Description); err != nil {
		return nil, err
	}
	if err := permission.WriteAuditLog(l.ctx, l.svcCtx.DB, permission.UserPrincipal(actorID), "permission.upsert", permission.Principal{Type: "permission", ID: 0}, strings.TrimSpace(req.Code), 0, "", permission.SystemScope(), "", map[string]any{
		"permission":   strings.TrimSpace(req.Code),
		"service_code": strings.TrimSpace(req.ServiceCode),
	}); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) DeletePermission(req *types.DeletePermissionReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req == nil || strings.TrimSpace(req.Code) == "" {
		return nil, errors.New("permission code is required")
	}
	if err := l.svcCtx.AdminRepo.DeletePermission(l.ctx, actorID, req.Code); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) UpsertResourceType(req *types.ResourceTypeManageReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req == nil || strings.TrimSpace(req.Code) == "" {
		return nil, errors.New("resource type code is required")
	}
	if err := permission.RegisterResourceType(l.ctx, l.svcCtx.DB, req.Code, req.ServiceCode, req.Name, req.Description); err != nil {
		return nil, err
	}
	if err := permission.WriteAuditLog(l.ctx, l.svcCtx.DB, permission.UserPrincipal(actorID), "resource_type.upsert", permission.Principal{Type: "resource_type", ID: 0}, "", 0, "", permission.SystemScope(), "", map[string]any{
		"resource_type": strings.TrimSpace(req.Code),
		"service_code":  strings.TrimSpace(req.ServiceCode),
	}); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) DeleteResourceType(req *types.DeleteResourceTypeReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req == nil || strings.TrimSpace(req.Code) == "" {
		return nil, errors.New("resource type code is required")
	}
	if err := l.svcCtx.AdminRepo.DeleteResourceType(l.ctx, actorID, req.Code); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) AddUserRole(req *types.UserRoleReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req.UserId <= 0 || strings.TrimSpace(req.Role) == "" {
		return nil, errors.New("invalid role request")
	}
	if err := l.svcCtx.AdminRepo.AddGlobalUserRole(l.ctx, actorID, req.UserId, strings.TrimSpace(req.Role)); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) RemoveUserRole(req *types.UserRoleReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req.UserId <= 0 || strings.TrimSpace(req.Role) == "" {
		return nil, errors.New("invalid role request")
	}
	if err := l.svcCtx.AdminRepo.RemoveGlobalUserRole(l.ctx, actorID, req.UserId, strings.TrimSpace(req.Role)); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) BindRole(req *types.RoleBindingReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	target, err := principalFromRoleBinding(req)
	if err != nil {
		return nil, err
	}
	scope := scopeFromStrings(req.ScopeType, req.ScopeId)
	expiresAt, err := parseOptionalRFC3339(req.ExpiresAt)
	if err != nil {
		return nil, err
	}
	if err := permission.BindRole(
		l.ctx,
		l.svcCtx.DB,
		permission.UserPrincipal(actorID),
		target,
		req.Role,
		scope,
		expiresAt,
	); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) UnbindRole(req *types.RoleBindingReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	target, err := principalFromRoleBinding(req)
	if err != nil {
		return nil, err
	}
	if err := permission.UnbindRole(
		l.ctx,
		l.svcCtx.DB,
		permission.UserPrincipal(actorID),
		target,
		strings.TrimSpace(req.Role),
		scopeFromStrings(req.ScopeType, req.ScopeId),
	); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) AssignPermission(req *types.PermissionAssignmentReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	target, err := principalFromPermissionAssignment(req)
	if err != nil {
		return nil, err
	}
	effect := strings.ToLower(strings.TrimSpace(req.Effect))
	if effect == "" {
		effect = permission.EffectAllow
	}
	expiresAt, err := parseOptionalRFC3339(req.ExpiresAt)
	if err != nil {
		return nil, err
	}
	if err := permission.AssignPermission(
		l.ctx,
		l.svcCtx.DB,
		permission.UserPrincipal(actorID),
		target,
		strings.TrimSpace(req.Permission),
		scopeFromStrings(req.ScopeType, req.ScopeId),
		effect,
		strings.TrimSpace(req.Reason),
		expiresAt,
	); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) RevokePermission(req *types.PermissionAssignmentReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	target, err := principalFromPermissionAssignment(req)
	if err != nil {
		return nil, err
	}
	if err := permission.RevokePermissionAssignment(
		l.ctx,
		l.svcCtx.DB,
		permission.UserPrincipal(actorID),
		target,
		strings.TrimSpace(req.Permission),
		scopeFromStrings(req.ScopeType, req.ScopeId),
	); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) AddResourceEdge(req *types.ResourceEdgeReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	parent, child, relation, err := resourceEdgeFromRequest(req)
	if err != nil {
		return nil, err
	}
	if err := permission.AddResourceEdge(l.ctx, l.svcCtx.DB, parent, child, relation); err != nil {
		return nil, err
	}
	if err := permission.WriteAuditLog(l.ctx, l.svcCtx.DB, permission.UserPrincipal(actorID), "resource_edge.add", permission.Principal{Type: "resource", ID: 0}, "", 0, "", permission.SystemScope(), "", map[string]any{
		"parent_type": parent.Type,
		"parent_id":   parent.ID,
		"child_type":  child.Type,
		"child_id":    child.ID,
		"relation":    relation,
	}); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) RemoveResourceEdge(req *types.ResourceEdgeReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	parent, child, relation, err := resourceEdgeFromRequest(req)
	if err != nil {
		return nil, err
	}
	if err := permission.RemoveResourceEdge(l.ctx, l.svcCtx.DB, parent, child, relation); err != nil {
		return nil, err
	}
	if err := permission.WriteAuditLog(l.ctx, l.svcCtx.DB, permission.UserPrincipal(actorID), "resource_edge.remove", permission.Principal{Type: "resource", ID: 0}, "", 0, "", permission.SystemScope(), "", map[string]any{
		"parent_type": parent.Type,
		"parent_id":   parent.ID,
		"child_type":  child.Type,
		"child_id":    child.ID,
		"relation":    relation,
	}); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) AddProblemRole(req *types.ProblemRoleReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req.UserId <= 0 || req.ProblemId <= 0 || strings.TrimSpace(req.Role) == "" {
		return nil, errors.New("invalid problem role request")
	}
	if err := permission.BindRole(
		l.ctx,
		l.svcCtx.DB,
		permission.UserPrincipal(actorID),
		permission.UserPrincipal(req.UserId),
		strings.TrimSpace(req.Role),
		permission.Scope{Type: "problem", ID: req.ProblemId},
		nil,
	); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) RemoveProblemRole(req *types.ProblemRoleReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req.UserId <= 0 || req.ProblemId <= 0 || strings.TrimSpace(req.Role) == "" {
		return nil, errors.New("invalid problem role request")
	}
	if err := l.svcCtx.AdminRepo.RemoveScopedRole(l.ctx, actorID, req.UserId, strings.TrimSpace(req.Role), "problem", req.ProblemId); err != nil {
		return nil, err
	}
	return okResp(), nil
}

func (l *AdminPermissionsLogic) CheckPermission(req *types.PermissionCheckReq) (*types.PermissionCheckResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if req.UserId <= 0 || strings.TrimSpace(req.Permission) == "" {
		return nil, errors.New("invalid permission check request")
	}
	scopeType := strings.TrimSpace(req.ScopeType)
	if scopeType == "" {
		scopeType = "system"
	}
	allowed, err := permission.HasUserPermission(
		l.ctx,
		l.svcCtx.DB,
		req.UserId,
		strings.TrimSpace(req.Permission),
		permission.Scope{Type: scopeType, ID: req.ScopeId},
	)
	if err != nil {
		return nil, err
	}
	if err := auditUserPermissionCheck(l.ctx, l.svcCtx, actorID, req.UserId, strings.TrimSpace(req.Permission), permission.Scope{Type: scopeType, ID: req.ScopeId}, allowed, "admin", strings.TrimSpace(req.ApiId)); err != nil {
		return nil, err
	}
	return &types.PermissionCheckResp{
		Code: 0,
		Msg:  "success",
		Data: types.PermissionCheckData{Allowed: allowed},
	}, nil
}

func (l *AdminPermissionsLogic) ListAuditLogs() (*types.ListAuditLogsResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	logs, err := l.svcCtx.AdminRepo.ListAuditLogs(l.ctx)
	if err != nil {
		return nil, err
	}
	items := make([]types.AuditLogItem, 0, len(logs))
	for _, item := range logs {
		items = append(items, types.AuditLogItem{
			Id:             item.ID,
			ActorType:      item.ActorType,
			ActorId:        item.ActorID,
			Action:         item.Action,
			TargetType:     item.TargetType,
			TargetId:       item.TargetID,
			PermissionCode: item.PermissionCode,
			RoleName:       item.RoleName,
			ScopeType:      item.ScopeType,
			ScopeId:        item.ScopeID,
			Effect:         item.Effect,
			CreatedAt:      item.CreatedAt.UTC().Format(time.RFC3339Nano),
		})
	}
	return &types.ListAuditLogsResp{Code: 0, Msg: "success", Data: items}, nil
}

func okResp() *types.AdminActionResp {
	return &types.AdminActionResp{
		Code: 0,
		Msg:  "success",
		Data: types.ActionData{Ok: true},
	}
}

func principalFromRoleBinding(req *types.RoleBindingReq) (permission.Principal, error) {
	if req == nil {
		return permission.Principal{}, errors.New("role binding request is required")
	}
	return principalFromParts(req.TargetType, req.TargetId)
}

func principalFromPermissionAssignment(req *types.PermissionAssignmentReq) (permission.Principal, error) {
	if req == nil {
		return permission.Principal{}, errors.New("permission assignment request is required")
	}
	return principalFromParts(req.TargetType, req.TargetId)
}

func principalFromParts(targetType string, targetID int64) (permission.Principal, error) {
	targetType = strings.ToLower(strings.TrimSpace(targetType))
	if targetType == "" {
		targetType = permission.PrincipalUser
	}
	if targetID <= 0 {
		return permission.Principal{}, errors.New("target_id is required")
	}
	switch targetType {
	case permission.PrincipalUser, permission.PrincipalTeam, permission.PrincipalGroup, permission.PrincipalService:
		return permission.Principal{Type: targetType, ID: targetID}, nil
	default:
		return permission.Principal{}, errors.New("unsupported target_type")
	}
}

func scopeFromStrings(scopeType string, scopeID int64) permission.Scope {
	scopeType = strings.TrimSpace(scopeType)
	if scopeType == "" {
		scopeType = permission.ScopeSystem
	}
	return permission.Scope{Type: scopeType, ID: scopeID}
}

func resourceEdgeFromRequest(req *types.ResourceEdgeReq) (permission.Scope, permission.Scope, string, error) {
	if req == nil {
		return permission.Scope{}, permission.Scope{}, "", errors.New("resource edge request is required")
	}
	parent := permission.Scope{Type: strings.TrimSpace(req.ParentType), ID: req.ParentId}
	child := permission.Scope{Type: strings.TrimSpace(req.ChildType), ID: req.ChildId}
	relation := strings.TrimSpace(req.Relation)
	if relation == "" {
		relation = "contains"
	}
	if parent.Type == "" || child.Type == "" {
		return permission.Scope{}, permission.Scope{}, "", errors.New("parent_type and child_type are required")
	}
	if parent.Type == child.Type && parent.ID == child.ID {
		return permission.Scope{}, permission.Scope{}, "", errors.New("resource edge cannot point to itself")
	}
	return parent, child, relation, nil
}

func parseOptionalRFC3339(value string) (*time.Time, error) {
	value = strings.TrimSpace(value)
	if value == "" {
		return nil, nil
	}
	parsed, err := time.Parse(time.RFC3339, value)
	if err != nil {
		return nil, err
	}
	return &parsed, nil
}
