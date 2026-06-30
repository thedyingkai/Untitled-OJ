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
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
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
