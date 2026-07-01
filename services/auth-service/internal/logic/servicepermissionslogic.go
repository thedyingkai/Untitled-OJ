package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-auth-service/internal/repository"
	"ojos-auth-service/internal/svc"
	"ojos-auth-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type ServicePermissionsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewServicePermissionsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ServicePermissionsLogic {
	return &ServicePermissionsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ServicePermissionsLogic) Register(req *types.RegisterServicePermissionsReq) (*types.ServicePermissionsResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	serviceCode := strings.TrimSpace(req.ServiceCode)
	if serviceCode == "" {
		return nil, errors.New("service_code is required")
	}
	if l.svcCtx.SmokeAuth != nil {
		permissions := make([]svc.SmokePermission, 0, len(req.Permissions))
		for _, item := range req.Permissions {
			permissions = append(permissions, svc.SmokePermission{
				Code:        item.Code,
				Name:        item.Name,
				Description: item.Description,
			})
		}
		identity := smokeIdentityFromRequest(req)
		registered := l.svcCtx.SmokeAuth.RegisterServicePermissions(serviceCode, permissions, identity)
		return &types.ServicePermissionsResp{
			Code: 0,
			Msg:  "success",
			Data: types.ServicePermissionsData{
				ServiceCode: serviceCode,
				Registered:  len(registered),
				Permissions: registered,
			},
		}, nil
	}
	permissions := make([]repository.ServicePermissionInput, 0, len(req.Permissions))
	for _, item := range req.Permissions {
		permissions = append(permissions, repository.ServicePermissionInput{
			Code:        item.Code,
			Name:        item.Name,
			Description: item.Description,
		})
	}
	bindings := make([]repository.ServiceRoleBindingInput, 0, len(req.DefaultRoleBindings))
	for _, binding := range req.DefaultRoleBindings {
		bindings = append(bindings, repository.ServiceRoleBindingInput{
			Role:        binding.Role,
			Permissions: binding.Permissions,
		})
	}
	identity := repositoryIdentityFromRequest(req)
	registered, err := l.svcCtx.AdminRepo.RegisterServicePermissions(l.ctx, serviceCode, permissions, bindings, identity)
	if err != nil {
		return nil, err
	}
	return &types.ServicePermissionsResp{
		Code: 0,
		Msg:  "success",
		Data: types.ServicePermissionsData{
			ServiceCode: serviceCode,
			Registered:  len(registered),
			Permissions: registered,
		},
	}, nil
}

func smokeIdentityFromRequest(req *types.RegisterServicePermissionsReq) *svc.SmokeServiceIdentity {
	if req == nil {
		return nil
	}
	if strings.TrimSpace(req.ServiceIdentity.ServiceName) == "" &&
		len(req.ServiceIdentity.AllowedApis) == 0 &&
		len(req.ServiceIdentity.Grants) == 0 {
		return nil
	}
	grants := make([]svc.SmokeServiceIdentityGrant, 0, len(req.ServiceIdentity.Grants))
	for _, grant := range req.ServiceIdentity.Grants {
		grants = append(grants, svc.SmokeServiceIdentityGrant{
			APIID:          grant.ApiId,
			PermissionCode: grant.Permission,
		})
	}
	return &svc.SmokeServiceIdentity{
		ServiceCode: req.ServiceIdentity.ServiceName,
		AllowedAPIs: req.ServiceIdentity.AllowedApis,
		Grants:      grants,
	}
}

func repositoryIdentityFromRequest(req *types.RegisterServicePermissionsReq) *repository.ServiceIdentityInput {
	if req == nil {
		return nil
	}
	if strings.TrimSpace(req.ServiceIdentity.ServiceName) == "" &&
		len(req.ServiceIdentity.AllowedApis) == 0 &&
		len(req.ServiceIdentity.Grants) == 0 {
		return nil
	}
	grants := make([]repository.ServiceIdentityGrantInput, 0, len(req.ServiceIdentity.Grants))
	for _, grant := range req.ServiceIdentity.Grants {
		grants = append(grants, repository.ServiceIdentityGrantInput{
			APIID:          grant.ApiId,
			PermissionCode: grant.Permission,
		})
	}
	return &repository.ServiceIdentityInput{
		ServiceCode: req.ServiceIdentity.ServiceName,
		AllowedAPIs: req.ServiceIdentity.AllowedApis,
		Grants:      grants,
	}
}

func (l *ServicePermissionsLogic) Delete(req *types.DeleteServicePermissionsReq) (*types.ServicePermissionsResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	serviceCode := strings.TrimSpace(req.ServiceCode)
	if l.svcCtx.SmokeAuth != nil {
		deleted := l.svcCtx.SmokeAuth.DeleteServicePermissions(serviceCode)
		return &types.ServicePermissionsResp{
			Code: 0,
			Msg:  "success",
			Data: types.ServicePermissionsData{
				ServiceCode: serviceCode,
				Deleted:     deleted,
				Permissions: []string{},
			},
		}, nil
	}
	deleted, err := l.svcCtx.AdminRepo.DeleteServicePermissions(l.ctx, serviceCode)
	if err != nil {
		return nil, err
	}
	return &types.ServicePermissionsResp{
		Code: 0,
		Msg:  "success",
		Data: types.ServicePermissionsData{
			ServiceCode: serviceCode,
			Deleted:     deleted,
			Permissions: []string{},
		},
	}, nil
}

func (l *ServicePermissionsLogic) UserEffective(req *types.UserEffectivePermissionsReq) (*types.UserEffectivePermissionsResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	scopeType := strings.TrimSpace(req.ScopeType)
	if scopeType == "" {
		scopeType = "system"
	}
	permissions, err := l.svcCtx.AdminRepo.UserEffectivePermissions(l.ctx, req.UserId, scopeType, req.ScopeId)
	if err != nil {
		return nil, err
	}
	return &types.UserEffectivePermissionsResp{
		Code: 0,
		Msg:  "success",
		Data: types.UserEffectivePermissionsData{
			UserId:      req.UserId,
			ScopeType:   scopeType,
			ScopeId:     req.ScopeId,
			Permissions: permissions,
		},
	}, nil
}
