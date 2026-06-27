package logic

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

const installerTokenHeader = "X-OJOS-Installer-Token"

type AdminModuleInstallerLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
	client *http.Client
}

func NewAdminModuleInstallerLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminModuleInstallerLogic {
	return &AdminModuleInstallerLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
		client: &http.Client{Timeout: 10 * time.Second},
	}
}

func (l *AdminModuleInstallerLogic) Discover(authHeader string) (*types.ModuleInstallerResp, error) {
	claims, err := requireAdminClaims(l.ctx, l.svcCtx, authHeader)
	if err != nil {
		return nil, err
	}
	return l.callInstaller(http.MethodGet, "/internal/modules/discover", nil, claims)
}

func (l *AdminModuleInstallerLogic) Validate(authHeader string, req *types.ModuleInstallerReq) (*types.ModuleInstallerResp, error) {
	claims, err := requireAdminClaims(l.ctx, l.svcCtx, authHeader)
	if err != nil {
		return nil, err
	}
	return l.callInstaller(http.MethodPost, "/internal/modules/validate", req, claims)
}

func (l *AdminModuleInstallerLogic) Plan(authHeader string, req *types.ModuleInstallerReq) (*types.ModuleInstallerResp, error) {
	claims, err := requireAdminClaims(l.ctx, l.svcCtx, authHeader)
	if err != nil {
		return nil, err
	}
	return l.callInstaller(http.MethodPost, "/internal/modules/plan", req, claims)
}

func (l *AdminModuleInstallerLogic) Install(authHeader string, req *types.ModuleInstallerReq) (*types.ModuleInstallerResp, error) {
	claims, err := requireAdminClaims(l.ctx, l.svcCtx, authHeader)
	if err != nil {
		return nil, err
	}
	return l.callInstaller(http.MethodPost, "/internal/modules/install", req, claims)
}

func (l *AdminModuleInstallerLogic) ModuleAction(authHeader string, moduleID string, suffix string, method string, body any) (*types.ModuleInstallerResp, error) {
	claims, err := requireAdminClaims(l.ctx, l.svcCtx, authHeader)
	if err != nil {
		return nil, err
	}
	moduleID = strings.TrimSpace(moduleID)
	if moduleID == "" {
		return nil, errors.New("module id is required")
	}
	return l.callInstaller(method, "/internal/modules/"+moduleID+suffix, body, claims)
}

func (l *AdminModuleInstallerLogic) callInstaller(method string, path string, body any, claims adminClaims) (*types.ModuleInstallerResp, error) {
	endpoint := strings.TrimRight(strings.TrimSpace(l.svcCtx.Config.Installer.Endpoint), "/")
	token := strings.TrimSpace(l.svcCtx.Config.Installer.InternalToken)
	if endpoint == "" || token == "" {
		return nil, errors.New("module installer is not configured")
	}

	var reader io.Reader
	if body != nil {
		data, err := json.Marshal(body)
		if err != nil {
			return nil, err
		}
		reader = bytes.NewReader(data)
	}

	httpReq, err := http.NewRequestWithContext(l.ctx, method, endpoint+path, reader)
	if err != nil {
		return nil, errors.New("module installer request build failed")
	}
	httpReq.Header.Set("Accept", "application/json")
	httpReq.Header.Set("Content-Type", "application/json; charset=utf-8")
	httpReq.Header.Set(installerTokenHeader, token)
	httpReq.Header.Set("X-User-Id", fmt.Sprintf("%d", claims.UserID))
	httpReq.Header.Set("X-Username", claims.Username)
	httpReq.Header.Set("X-Roles", strings.Join(claims.Roles, ","))

	resp, err := l.client.Do(httpReq)
	if err != nil {
		return nil, errors.New("module installer unavailable")
	}
	defer resp.Body.Close()

	data, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return nil, errors.New("module installer response read failed")
	}

	var payload types.ModuleInstallerResp
	if err := json.Unmarshal(data, &payload); err != nil {
		return nil, errors.New("module installer returned invalid response")
	}
	if payload.Code != 0 {
		return nil, installerStatusError(resp.StatusCode, payload.Msg)
	}
	if resp.StatusCode >= 400 {
		return nil, installerStatusError(resp.StatusCode, payload.Msg)
	}
	return &payload, nil
}

func installerStatusError(status int, msg string) error {
	msg = strings.TrimSpace(msg)
	if msg == "" {
		msg = "module installer request failed"
	}
	switch status {
	case http.StatusUnauthorized:
		return errors.New("unauthorized: " + msg)
	case http.StatusForbidden:
		return errors.New("forbidden: " + msg)
	case http.StatusNotFound:
		return errors.New("not found: " + msg)
	default:
		return errors.New(msg)
	}
}
