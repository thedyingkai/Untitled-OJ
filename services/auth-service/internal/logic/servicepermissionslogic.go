package logic

import (
	"context"
	"strings"

	"ojos-auth-service/internal/apperror"
	"ojos-auth-service/internal/middleware"
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
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	serviceCode := strings.TrimSpace(req.ServiceCode)
	if serviceCode == "" {
		return nil, apperror.BadRequest(apperror.CodeInvalidRequest, "service_code is required")
	}
	authToken, _ := middleware.TokenFromContext(l.ctx)
	credentialToken := credentialTokenFromRegistration(req, authToken)
	if l.svcCtx.SmokeAuth != nil {
		permissions := make([]svc.SmokePermission, 0, len(req.Permissions))
		for _, item := range req.Permissions {
			permissions = append(permissions, svc.SmokePermission{
				Code:        item.Code,
				Name:        item.Name,
				Description: item.Description,
			})
		}
		identity := smokeIdentityFromRequest(req, credentialToken)
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
	identity, err := repositoryIdentityFromRequest(req, credentialToken)
	if err != nil {
		return nil, err
	}
	registered, err := l.svcCtx.AdminRepo.RegisterServicePermissions(l.ctx, actorID, serviceCode, permissions, bindings, identity)
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

func smokeIdentityFromRequest(req *types.RegisterServicePermissionsReq, credentialToken string) *svc.SmokeServiceIdentity {
	if req == nil {
		return nil
	}
	if strings.TrimSpace(req.ServiceIdentity.ServiceName) == "" &&
		len(req.ServiceIdentity.AllowedApis) == 0 &&
		len(req.ServiceIdentity.Grants) == 0 &&
		strings.TrimSpace(req.ServiceIdentity.CredentialToken) == "" {
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
		ServiceCode:     req.ServiceIdentity.ServiceName,
		AllowedAPIs:     req.ServiceIdentity.AllowedApis,
		Grants:          grants,
		CredentialToken: credentialToken,
	}
}

func repositoryIdentityFromRequest(req *types.RegisterServicePermissionsReq, credentialToken string) (*repository.ServiceIdentityInput, error) {
	if req == nil {
		return nil, nil
	}
	if strings.TrimSpace(req.ServiceIdentity.ServiceName) == "" &&
		len(req.ServiceIdentity.AllowedApis) == 0 &&
		len(req.ServiceIdentity.Grants) == 0 &&
		strings.TrimSpace(req.ServiceIdentity.CredentialToken) == "" {
		return nil, nil
	}
	grants := make([]repository.ServiceIdentityGrantInput, 0, len(req.ServiceIdentity.Grants))
	for _, grant := range req.ServiceIdentity.Grants {
		grants = append(grants, repository.ServiceIdentityGrantInput{
			APIID:          grant.ApiId,
			PermissionCode: grant.Permission,
		})
	}
	expiresAt, err := parseOptionalRFC3339(req.ServiceIdentity.CredentialExpiresAt)
	if err != nil {
		return nil, err
	}
	return &repository.ServiceIdentityInput{
		ServiceCode:         req.ServiceIdentity.ServiceName,
		AllowedAPIs:         req.ServiceIdentity.AllowedApis,
		Grants:              grants,
		CredentialToken:     credentialToken,
		CredentialExpiresAt: expiresAt,
	}, nil
}

func (l *ServicePermissionsLogic) Delete(req *types.DeleteServicePermissionsReq) (*types.ServicePermissionsResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
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
	deleted, err := l.svcCtx.AdminRepo.DeleteServicePermissions(l.ctx, actorID, serviceCode)
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

func (l *ServicePermissionsLogic) GetServiceIdentity(req *types.DeleteServicePermissionsReq) (*types.ServiceIdentityResp, error) {
	if _, err := requireAdmin(l.ctx, l.svcCtx); err != nil {
		return nil, err
	}
	if l.svcCtx.SmokeAuth != nil {
		return nil, apperror.BadRequest(apperror.CodeInvalidRequest, "service identity details are unavailable in smoke auth")
	}
	details, err := l.svcCtx.AdminRepo.ListServiceIdentity(l.ctx, strings.TrimSpace(req.ServiceCode))
	if err != nil {
		return nil, err
	}
	return &types.ServiceIdentityResp{
		Code: 0,
		Msg:  "success",
		Data: serviceIdentityDataFromRepository(details),
	}, nil
}

func (l *ServicePermissionsLogic) AddServiceCredential(req *types.ServiceCredentialReq) (*types.ServiceCredentialResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if l.svcCtx.SmokeAuth != nil {
		return nil, apperror.BadRequest(apperror.CodeInvalidRequest, "service credential lifecycle is unavailable in smoke auth")
	}
	expiresAt, err := parseOptionalRFC3339(req.ExpiresAt)
	if err != nil {
		return nil, err
	}
	item, err := l.svcCtx.AdminRepo.AddServiceCredential(l.ctx, actorID, strings.TrimSpace(req.ServiceCode), repository.ServiceCredentialInput{
		Token:     req.Token,
		ExpiresAt: expiresAt,
	})
	if err != nil {
		return nil, err
	}
	return &types.ServiceCredentialResp{
		Code: 0,
		Msg:  "success",
		Data: serviceCredentialItemFromRepository(item),
	}, nil
}

func (l *ServicePermissionsLogic) RevokeServiceCredential(req *types.RevokeServiceCredentialReq) (*types.AdminActionResp, error) {
	actorID, err := requireAdmin(l.ctx, l.svcCtx)
	if err != nil {
		return nil, err
	}
	if l.svcCtx.SmokeAuth != nil {
		return nil, apperror.BadRequest(apperror.CodeInvalidRequest, "service credential lifecycle is unavailable in smoke auth")
	}
	if err := l.svcCtx.AdminRepo.RevokeServiceCredential(l.ctx, actorID, strings.TrimSpace(req.ServiceCode), req.Token, req.TokenHash, req.Reason); err != nil {
		return nil, err
	}
	return okResp(), nil
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

func credentialTokenFromRegistration(req *types.RegisterServicePermissionsReq, fallback string) string {
	if req == nil {
		return fallback
	}
	if token := strings.TrimSpace(req.ServiceIdentity.CredentialToken); token != "" {
		return token
	}
	return fallback
}

func serviceIdentityDataFromRepository(item repository.ServiceIdentityDetails) types.ServiceIdentityData {
	grants := make([]types.ServiceGrantData, 0, len(item.Grants))
	for _, grant := range item.Grants {
		grants = append(grants, types.ServiceGrantData{
			ApiId:               grant.APIID,
			Permission:          grant.PermissionCode,
			ProviderServiceCode: grant.ProviderServiceCode,
			Enabled:             grant.Enabled,
		})
	}
	credentials := make([]types.ServiceCredentialItem, 0, len(item.Credentials))
	for _, credential := range item.Credentials {
		credentials = append(credentials, serviceCredentialItemFromRepository(credential))
	}
	return types.ServiceIdentityData{
		ServiceCode: item.ServiceCode,
		Enabled:     item.Enabled,
		Grants:      grants,
		Credentials: credentials,
	}
}

func serviceCredentialItemFromRepository(item repository.ServiceCredentialListItem) types.ServiceCredentialItem {
	return types.ServiceCredentialItem{
		ServiceCode: item.ServiceCode,
		TokenHint:   item.TokenHint,
		Enabled:     item.Enabled,
		CreatedAt:   item.CreatedAt,
		UpdatedAt:   item.UpdatedAt,
		ExpiresAt:   item.ExpiresAt,
		RevokedAt:   item.RevokedAt,
		LastUsedAt:  item.LastUsedAt,
	}
}
