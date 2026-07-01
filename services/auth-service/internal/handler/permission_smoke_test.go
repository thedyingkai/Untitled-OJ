package handler

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"ojos-auth-service/internal/config"
	authmw "ojos-auth-service/internal/middleware"
	"ojos-auth-service/internal/svc"
)

func TestSmokePermissionCheckServiceCallerBoundaries(t *testing.T) {
	svcCtx := &svc.ServiceContext{
		Config:         config.Config{},
		SmokeAuth:      svc.NewSmokePermissionStore(),
		AuthMiddleware: authmw.NewAuthMiddleware("smoke", "smoke-token").Handle,
	}
	svcCtx.SmokeAuth.Allow("judge-worker", "storage.object.read")
	handler := svcCtx.AuthMiddleware(userPermissionCheckHandler(svcCtx))

	allowed := permissionCheckRequest("smoke-token", "judge-worker", "storage.object.read", handler)
	if allowed.status != http.StatusOK || !allowed.allowed {
		t.Fatalf("allowed service caller got status=%d allowed=%v body=%s", allowed.status, allowed.allowed, allowed.body)
	}

	missing := permissionCheckRequest("", "judge-worker", "storage.object.read", handler)
	if missing.status != http.StatusUnauthorized {
		t.Fatalf("missing token got status=%d body=%s", missing.status, missing.body)
	}

	denied := permissionCheckRequest("smoke-token", "judge-worker", "storage.object.delete", handler)
	if denied.status != http.StatusOK || denied.allowed {
		t.Fatalf("denied service caller got status=%d allowed=%v body=%s", denied.status, denied.allowed, denied.body)
	}
}

type permissionCheckTestResult struct {
	status  int
	allowed bool
	body    string
}

func permissionCheckRequest(token string, service string, permission string, handler http.HandlerFunc) permissionCheckTestResult {
	payload, _ := json.Marshal(map[string]any{
		"caller_type":    "service",
		"caller_service": service,
		"permission":     permission,
		"scope_type":     "system",
	})
	req := httptest.NewRequest(http.MethodPost, "/auth/permission-check", bytes.NewReader(payload))
	req.Header.Set("Content-Type", "application/json")
	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}
	rr := httptest.NewRecorder()
	handler(rr, req)

	var decoded struct {
		Data struct {
			Allowed bool `json:"allowed"`
		} `json:"data"`
	}
	_ = json.Unmarshal(rr.Body.Bytes(), &decoded)
	return permissionCheckTestResult{
		status:  rr.Code,
		allowed: decoded.Data.Allowed,
		body:    rr.Body.String(),
	}
}
