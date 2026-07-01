package authclient

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"
)

type Client struct {
	baseURL string
	http    *http.Client
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

func New(baseURL string) *Client {
	return &Client{
		baseURL: strings.TrimRight(strings.TrimSpace(baseURL), "/"),
		http: &http.Client{
			Timeout: 2 * time.Second,
		},
	}
}

func (c *Client) Configured() bool {
	return c != nil && c.baseURL != ""
}

func (c *Client) HasSystemPermission(ctx context.Context, authHeader string, userID int64, permission string) (bool, error) {
	if !c.Configured() {
		return false, errors.New("auth-service permission client is not configured")
	}
	permission = strings.TrimSpace(permission)
	if userID <= 0 || permission == "" {
		return false, nil
	}

	body, err := json.Marshal(permissionCheckRequest{
		UserID:     userID,
		Permission: permission,
		ScopeType:  "system",
		ScopeID:    0,
	})
	if err != nil {
		return false, err
	}

	req, err := http.NewRequestWithContext(
		ctx,
		http.MethodPost,
		c.baseURL+"/auth/permission-check",
		bytes.NewReader(body),
	)
	if err != nil {
		return false, err
	}
	req.Header.Set("Content-Type", "application/json")
	if strings.TrimSpace(authHeader) != "" {
		req.Header.Set("Authorization", strings.TrimSpace(authHeader))
	}

	resp, err := c.http.Do(req)
	if err != nil {
		return false, err
	}
	defer resp.Body.Close()

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return false, fmt.Errorf("auth-service permission check returned %s", resp.Status)
	}

	var decoded permissionCheckResponse
	if err := json.NewDecoder(resp.Body).Decode(&decoded); err != nil {
		return false, err
	}
	if decoded.Code != 0 {
		if decoded.Msg == "" {
			decoded.Msg = "permission check failed"
		}
		return false, errors.New(decoded.Msg)
	}
	return decoded.Data.Allowed, nil
}
