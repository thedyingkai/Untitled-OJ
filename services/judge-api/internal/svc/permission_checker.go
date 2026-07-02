package svc

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"

	sharedperm "ojos-shared/security/permission"

	"github.com/jackc/pgx/v5/pgxpool"
)

type databasePermissionChecker struct {
	db *pgxpool.Pool
}

func (p databasePermissionChecker) RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) error {
	return sharedperm.RequireUserPermission(ctx, p.db, userID, permissionCode, scope)
}

func (p databasePermissionChecker) HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) (bool, error) {
	return sharedperm.HasUserPermission(ctx, p.db, userID, permissionCode, scope)
}

type authServicePermissionChecker struct {
	endpoint   string
	adminToken string
	client     *http.Client
}

type permissionCheckRequest struct {
	UserID     int64  `json:"user_id"`
	Permission string `json:"permission"`
	ScopeType  string `json:"scope_type,omitempty"`
	ScopeID    int64  `json:"scope_id,omitempty"`
}

type permissionCheckResponse struct {
	Code int    `json:"code"`
	Msg  string `json:"msg"`
	Data struct {
		Allowed bool `json:"allowed"`
	} `json:"data"`
}

func newAuthServicePermissionChecker(endpoint string, adminToken string) PermissionChecker {
	endpoint = strings.TrimRight(strings.TrimSpace(endpoint), "/")
	adminToken = strings.TrimSpace(adminToken)
	if endpoint == "" || adminToken == "" {
		return nil
	}
	return authServicePermissionChecker{
		endpoint:   endpoint,
		adminToken: adminToken,
		client: &http.Client{
			Timeout: 5 * time.Second,
		},
	}
}

func (p authServicePermissionChecker) RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) error {
	allowed, err := p.HasUserPermission(ctx, userID, permissionCode, scope)
	if err != nil {
		return err
	}
	if !allowed {
		return sharedperm.ErrForbidden
	}
	return nil
}

func (p authServicePermissionChecker) HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope sharedperm.Scope) (bool, error) {
	permissionCode = strings.TrimSpace(permissionCode)
	if userID <= 0 {
		return false, errors.New("invalid user id")
	}
	if permissionCode == "" {
		return false, errors.New("permission code is empty")
	}
	scopeType := strings.TrimSpace(scope.Type)
	if scopeType == "" {
		scopeType = sharedperm.ScopeSystem
	}

	body, err := json.Marshal(permissionCheckRequest{
		UserID:     userID,
		Permission: permissionCode,
		ScopeType:  scopeType,
		ScopeID:    scope.ID,
	})
	if err != nil {
		return false, err
	}

	req, err := http.NewRequestWithContext(
		ctx,
		http.MethodPost,
		p.endpoint+"/auth/admin/permission-check",
		bytes.NewReader(body),
	)
	if err != nil {
		return false, err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bearer "+p.adminToken)

	resp, err := p.client.Do(req)
	if err != nil {
		return false, err
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusUnauthorized {
		return false, errors.New("auth-service permission check unauthorized")
	}
	if resp.StatusCode == http.StatusForbidden {
		return false, sharedperm.ErrForbidden
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return false, fmt.Errorf("auth-service permission check returned %s", resp.Status)
	}

	var decoded permissionCheckResponse
	if err := json.NewDecoder(resp.Body).Decode(&decoded); err != nil {
		return false, err
	}
	if decoded.Code != 0 {
		if strings.TrimSpace(decoded.Msg) == "" {
			decoded.Msg = "permission check failed"
		}
		return false, errors.New(decoded.Msg)
	}
	return decoded.Data.Allowed, nil
}
