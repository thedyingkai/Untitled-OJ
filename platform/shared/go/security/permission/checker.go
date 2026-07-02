package permission

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
)

type UserChecker interface {
	RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) error
	HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) (bool, error)
}

type DatabaseUserChecker struct {
	DB *pgxpool.Pool
}

func NewDatabaseUserChecker(db *pgxpool.Pool) UserChecker {
	if db == nil {
		return nil
	}
	return DatabaseUserChecker{DB: db}
}

func (p DatabaseUserChecker) RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) error {
	if p.DB == nil {
		return errors.New("permission database is not configured")
	}
	return RequireUserPermission(ctx, p.DB, userID, permissionCode, scope)
}

func (p DatabaseUserChecker) HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) (bool, error) {
	if p.DB == nil {
		return false, errors.New("permission database is not configured")
	}
	return HasUserPermission(ctx, p.DB, userID, permissionCode, scope)
}

type AuthServiceUserChecker struct {
	endpoint   string
	adminToken string
	client     *http.Client
}

type permissionCheckRequest struct {
	UserID     int64  `json:"user_id,omitempty"`
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

func NewAuthServiceUserChecker(endpoint string, adminToken string) UserChecker {
	endpoint = strings.TrimRight(strings.TrimSpace(endpoint), "/")
	adminToken = strings.TrimSpace(adminToken)
	if endpoint == "" || adminToken == "" {
		return nil
	}
	return AuthServiceUserChecker{
		endpoint:   endpoint,
		adminToken: adminToken,
		client: &http.Client{
			Timeout: 5 * time.Second,
		},
	}
}

func NewUserChecker(authServiceEndpoint string, authServiceAdminToken string, db *pgxpool.Pool) UserChecker {
	if checker := NewAuthServiceUserChecker(authServiceEndpoint, authServiceAdminToken); checker != nil {
		return checker
	}
	return NewDatabaseUserChecker(db)
}

func (p AuthServiceUserChecker) RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) error {
	allowed, err := p.HasUserPermission(ctx, userID, permissionCode, scope)
	if err != nil {
		return err
	}
	if !allowed {
		return ErrForbidden
	}
	return nil
}

func (p AuthServiceUserChecker) HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) (bool, error) {
	permissionCode = strings.TrimSpace(permissionCode)
	if userID <= 0 {
		return false, errors.New("invalid user id")
	}
	if permissionCode == "" {
		return false, errors.New("permission code is empty")
	}

	scope = normalizeScope(scope)
	body, err := json.Marshal(permissionCheckRequest{
		UserID:     userID,
		Permission: permissionCode,
		ScopeType:  scope.Type,
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
		return false, ErrForbidden
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
