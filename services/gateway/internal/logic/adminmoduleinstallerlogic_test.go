package logic

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/svc"
	sharedjwt "ojos-shared/security/jwt"
)

func TestInstallerDiscoverRejectsNoTokenAndOrdinaryUser(t *testing.T) {
	oldChecker := hasSystemAdminPermission
	hasSystemAdminPermission = func(context.Context, *svc.ServiceContext, int64) (bool, error) {
		return false, nil
	}
	defer func() { hasSystemAdminPermission = oldChecker }()

	logic := NewAdminModuleInstallerLogic(context.Background(), testInstallerSvc("http://127.0.0.1:1"))
	if _, err := logic.Discover(""); err == nil || !strings.Contains(err.Error(), "missing authorization") {
		t.Fatalf("expected missing authorization, got %v", err)
	}

	token, err := sharedjwt.Generate("test-secret", 42, "alice", []string{"user"}, 1)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := logic.Discover("Bearer " + token); err == nil || !strings.Contains(err.Error(), "forbidden") {
		t.Fatalf("expected forbidden, got %v", err)
	}
}

func TestInstallerActorPropagationAndSuccess(t *testing.T) {
	var gotUserID string
	var gotUsername string
	var gotToken string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotUserID = r.Header.Get("X-User-Id")
		gotUsername = r.Header.Get("X-Username")
		gotToken = r.Header.Get(installerTokenHeader)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"code": 0,
			"msg":  "success",
			"data": map[string]any{"ok": true},
		})
	}))
	defer server.Close()

	token, err := sharedjwt.Generate("test-secret", 7, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	logic := NewAdminModuleInstallerLogic(context.Background(), testInstallerSvc(server.URL))
	resp, err := logic.Discover("Bearer " + token)
	if err != nil {
		t.Fatalf("discover failed: %v", err)
	}
	if resp.Code != 0 {
		t.Fatalf("unexpected response: %#v", resp)
	}
	if gotUserID != "7" || gotUsername != "root" || gotToken != "test-token" {
		t.Fatalf("actor/token not propagated: user=%s username=%s token=%s", gotUserID, gotUsername, gotToken)
	}
}

func TestInstallerInternalErrorMapping(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusBadRequest)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"code": 40010,
			"msg":  "install plan is blocked",
		})
	}))
	defer server.Close()

	token, err := sharedjwt.Generate("test-secret", 7, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	logic := NewAdminModuleInstallerLogic(context.Background(), testInstallerSvc(server.URL))
	_, err = logic.Discover("Bearer " + token)
	if err == nil || !strings.Contains(err.Error(), "install plan is blocked") {
		t.Fatalf("expected mapped installer error, got %v", err)
	}
}

func TestInstallerStructuredErrorMapping(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusConflict)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"error": map[string]any{
				"code":     "OPERATION_LOCK_HELD",
				"message":  "module operation lock is held",
				"severity": "error",
				"details":  map[string]any{},
			},
		})
	}))
	defer server.Close()

	token, err := sharedjwt.Generate("test-secret", 7, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	logic := NewAdminModuleInstallerLogic(context.Background(), testInstallerSvc(server.URL))
	_, err = logic.Discover("Bearer " + token)
	if err == nil || !strings.Contains(err.Error(), "conflict: module operation lock is held") {
		t.Fatalf("expected conflict installer error, got %v", err)
	}
}

func TestInstallerUnavailableMapping(t *testing.T) {
	token, err := sharedjwt.Generate("test-secret", 7, "root", []string{"admin"}, 1)
	if err != nil {
		t.Fatal(err)
	}

	logic := NewAdminModuleInstallerLogic(context.Background(), testInstallerSvc("http://127.0.0.1:1"))
	_, err = logic.Discover("Bearer " + token)
	if err == nil || !strings.Contains(err.Error(), "service unavailable") {
		t.Fatalf("expected service unavailable, got %v", err)
	}
}

func testInstallerSvc(endpoint string) *svc.ServiceContext {
	return &svc.ServiceContext{
		Config: config.Config{
			Jwt: config.JwtConfig{Secret: "test-secret"},
			Installer: config.InstallerConfig{
				Endpoint:      endpoint,
				InternalToken: "test-token",
			},
		},
	}
}
