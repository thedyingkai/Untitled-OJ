package handler

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/zeromicro/go-zero/rest/pathvar"
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
	register := svcCtx.AuthMiddleware(registerServicePermissionsHandler(svcCtx))
	registerResult := registerSmokeStoragePermissions(register)
	if registerResult.status != http.StatusOK {
		t.Fatalf("register storage permissions got status=%d body=%s", registerResult.status, registerResult.body)
	}
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

	list := svcCtx.AuthMiddleware(listPermissionsHandler(svcCtx))
	req := httptest.NewRequest(http.MethodGet, "/auth/admin/permissions", nil)
	req.Header.Set("Authorization", "Bearer smoke-token")
	rr := httptest.NewRecorder()
	list(rr, req)
	if rr.Code != http.StatusOK || !strings.Contains(rr.Body.String(), "storage.object.read") {
		t.Fatalf("registered smoke permissions were not listable: status=%d body=%s", rr.Code, rr.Body.String())
	}
}

type permissionCheckTestResult struct {
	status  int
	allowed bool
	body    string
}

type registerSmokeResult struct {
	status int
	body   string
}

func registerSmokeStoragePermissions(handler http.HandlerFunc) registerSmokeResult {
	payload, _ := json.Marshal(map[string]any{
		"permissions": []map[string]any{
			{"code": "storage.object.read", "name": "storage.object.read"},
			{"code": "storage.object.write", "name": "storage.object.write"},
			{"code": "storage.object.delete", "name": "storage.object.delete"},
		},
		"default_role_bindings": []any{},
	})
	req := httptest.NewRequest(http.MethodPost, "/auth/admin/services/storage-service/permissions", bytes.NewReader(payload))
	req = pathvar.WithVars(req, map[string]string{"service_code": "storage-service"})
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bearer smoke-token")
	rr := httptest.NewRecorder()
	handler(rr, req)
	return registerSmokeResult{status: rr.Code, body: rr.Body.String()}
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
