package proxy

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/orchestrator/servicestatus"
	sharedjwt "ojos-shared/security/jwt"

	"go.uber.org/zap"
)

func TestServiceProxyUsesTrustedServiceAndStripsAuthorization(t *testing.T) {
	var gotAuth string
	var gotConnection string
	var gotPath string
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotAuth = r.Header.Get("Authorization")
		gotConnection = r.Header.Get("Connection")
		gotPath = r.URL.Path
		if r.Header.Get("X-Auth-Verified") != "true" || r.Header.Get("X-User-Id") != "42" {
			t.Fatalf("sanitized actor headers not forwarded: %#v", r.Header)
		}
		_ = json.NewEncoder(w).Encode(map[string]string{"ok": "true"})
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, []config.ProxyTrustedServiceConfig{{
		ServiceID:   "demo-api",
		Target:      upstream.URL,
		StripPrefix: "/api",
	}})
	rp.SetRouteTable(servicestatus.RouteTable{
		Routes: []servicestatus.ServiceRoute{{
			RouteID:        "demo:/api/demo",
			OwnerServiceID: "demo",
			Prefix:         "/api/demo",
			ServiceID:      "demo-api",
			AuthMode:       "user",
			Enabled:        true,
			ProxyEnabled:   true,
			Status:         "active",
		}},
		CanProxy: true,
	})

	req := httptest.NewRequest(http.MethodGet, "/api/demo/ping", nil)
	req.Header.Set("Authorization", "Bearer "+testToken(t, []string{"user"}))
	req.Header.Set("Connection", "close")
	rr := httptest.NewRecorder()

	rp.ServeHTTP(rr, req)

	if rr.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d body=%s", rr.Code, rr.Body.String())
	}
	if gotAuth != "" {
		t.Fatalf("raw Authorization should not be forwarded, got %q", gotAuth)
	}
	if gotConnection != "" {
		t.Fatalf("hop-by-hop Connection header should be stripped, got %q", gotConnection)
	}
	if gotPath != "/demo/ping" {
		t.Fatalf("unexpected upstream path: %s", gotPath)
	}
}

func TestServiceProxyAuthModes(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	cases := []struct {
		name       string
		authMode   string
		tokenRoles []string
		wantStatus int
	}{
		{name: "public", authMode: "public", wantStatus: http.StatusNoContent},
		{name: "user missing token", authMode: "user", wantStatus: http.StatusUnauthorized},
		{name: "user ok", authMode: "user", tokenRoles: []string{"user"}, wantStatus: http.StatusNoContent},
		{name: "admin rejects ordinary user", authMode: "admin", tokenRoles: []string{"user"}, wantStatus: http.StatusForbidden},
		{name: "admin ok", authMode: "admin", tokenRoles: []string{"admin"}, wantStatus: http.StatusNoContent},
		{name: "worker rejected", authMode: "worker", tokenRoles: []string{"admin"}, wantStatus: http.StatusForbidden},
		{name: "internal rejected", authMode: "internal", tokenRoles: []string{"admin"}, wantStatus: http.StatusForbidden},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			rp := newTestServiceProxy(t, []config.ProxyTrustedServiceConfig{{
				ServiceID:   "demo-api",
				Target:      upstream.URL,
				StripPrefix: "/api",
			}})
			rp.SetRouteTable(servicestatus.RouteTable{
				Routes: []servicestatus.ServiceRoute{{
					RouteID:        "demo:/api/demo",
					OwnerServiceID: "demo",
					Prefix:         "/api/demo",
					ServiceID:      "demo-api",
					AuthMode:       tc.authMode,
					Enabled:        true,
					ProxyEnabled:   true,
					Status:         "active",
				}},
				CanProxy: true,
			})

			req := httptest.NewRequest(http.MethodGet, "/api/demo/ping", nil)
			if len(tc.tokenRoles) > 0 {
				req.Header.Set("Authorization", "Bearer "+testToken(t, tc.tokenRoles))
			}
			rr := httptest.NewRecorder()
			rp.ServeHTTP(rr, req)
			if rr.Code != tc.wantStatus {
				t.Fatalf("expected %d, got %d body=%s", tc.wantStatus, rr.Code, rr.Body.String())
			}
		})
	}
}

func TestServiceProxyAdminAuthCanUsePermissionChecker(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, []config.ProxyTrustedServiceConfig{{
		ServiceID:   "admin-api",
		Target:      upstream.URL,
		StripPrefix: "/api",
	}})
	rp.SetAdminChecker(func(ctx context.Context, userID int64) (bool, error) {
		return userID == 42, nil
	})
	rp.SetRouteTable(servicestatus.RouteTable{
		Routes: []servicestatus.ServiceRoute{{
			RouteID:      "demo:/api/admin-demo",
			Prefix:       "/api/admin-demo",
			ServiceID:    "admin-api",
			AuthMode:     "admin",
			Enabled:      true,
			ProxyEnabled: true,
			Status:       "active",
		}},
		CanProxy: true,
	})

	req := httptest.NewRequest(http.MethodGet, "/api/admin-demo/ping", nil)
	req.Header.Set("Authorization", "Bearer "+testToken(t, []string{"user"}))
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("expected permission checker to allow admin route, got %d body=%s", rr.Code, rr.Body.String())
	}
}

func TestServiceProxyRejectsUnknownServiceAndPrefersStaticRoute(t *testing.T) {
	staticUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte("static"))
	}))
	defer staticUpstream.Close()
	dynamicUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte("dynamic"))
	}))
	defer dynamicUpstream.Close()

	rp, err := NewServiceProxy([]config.ProxyRouteConfig{{
		Prefix:      "/api/auth",
		Target:      staticUpstream.URL,
		StripPrefix: "/api",
		AuthMode:    "public",
	}}, []config.ProxyTrustedServiceConfig{{
		ServiceID:   "demo-api",
		Target:      dynamicUpstream.URL,
		StripPrefix: "/api",
	}}, testSecret, nil, zap.NewNop())
	if err != nil {
		t.Fatal(err)
	}
	rp.SetRouteTable(servicestatus.RouteTable{
		Routes: []servicestatus.ServiceRoute{
			{RouteID: "demo:/api/auth", Prefix: "/api/auth", ServiceID: "demo-api", AuthMode: "public", Enabled: true, ProxyEnabled: true, Status: "active"},
			{RouteID: "bad:/api/bad", Prefix: "/api/bad", ServiceID: "missing", AuthMode: "public", Enabled: true, ProxyEnabled: true, Status: "active"},
		},
		CanProxy: true,
	})

	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/api/auth/ping", nil))
	if rr.Body.String() != "static" {
		t.Fatalf("core static route should win over dynamic route, got %q", rr.Body.String())
	}

	rr = httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/api/bad/ping", nil))
	if rr.Code != http.StatusNotFound {
		t.Fatalf("unknown service route should not proxy, got %d", rr.Code)
	}
}

func TestServiceProxyUnavailableServiceRouteReturnsStableError(t *testing.T) {
	staticUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte("static"))
	}))
	defer staticUpstream.Close()

	rp, err := NewServiceProxy([]config.ProxyRouteConfig{{
		Prefix:      "/api/problem",
		Target:      staticUpstream.URL,
		StripPrefix: "/api",
		AuthMode:    "required",
	}}, []config.ProxyTrustedServiceConfig{{
		ServiceID:   "problem-service",
		Target:      staticUpstream.URL,
		StripPrefix: "/api",
	}}, testSecret, nil, zap.NewNop())
	if err != nil {
		t.Fatal(err)
	}
	rp.SetRouteTable(servicestatus.RouteTable{
		Routes: []servicestatus.ServiceRoute{{
			RouteID:       "ojos.judge-core:/api/problem",
			Prefix:        "/api/problem",
			ServiceID:     "problem-service",
			AuthMode:      "user",
			Enabled:       true,
			ProxyEnabled:  false,
			Status:        "unavailable",
			ServiceStatus: servicestatus.ServiceStatusStopped,
			BlockedBy:     []string{"service not running"},
		}},
	})

	req := httptest.NewRequest(http.MethodGet, "/api/problem", nil)
	req.Header.Set("Authorization", "Bearer "+testToken(t, []string{"user"}))
	rr := httptest.NewRecorder()

	rp.ServeHTTP(rr, req)

	if rr.Code != http.StatusServiceUnavailable {
		t.Fatalf("expected 503, got %d body=%s", rr.Code, rr.Body.String())
	}
	if !strings.Contains(rr.Body.String(), "service unavailable") {
		t.Fatalf("expected stable service unavailable error, got %s", rr.Body.String())
	}
}

func TestServiceProxyReloadAtomicallyReplacesTable(t *testing.T) {
	reader := fakeServiceRouteReader{table: servicestatus.RouteTable{
		Version: "2",
		Routes: []servicestatus.ServiceRoute{{
			RouteID:      "demo:/api/demo",
			Prefix:       "/api/demo",
			ServiceID:    "demo-api",
			AuthMode:     "public",
			Enabled:      true,
			ProxyEnabled: true,
			Status:       "active",
		}},
		CanProxy: true,
	}}
	rp := newTestServiceProxy(t, []config.ProxyTrustedServiceConfig{{
		ServiceID: "demo-api",
		Target:    "http://demo-api:8080",
	}})
	table, err := rp.Reload(context.Background(), reader)
	if err != nil {
		t.Fatal(err)
	}
	if table.Version != "2" {
		t.Fatalf("unexpected table version %s", table.Version)
	}
}

type fakeServiceRouteReader struct {
	table servicestatus.RouteTable
}

func (f fakeServiceRouteReader) ServiceRouteTable(context.Context) (servicestatus.RouteTable, error) {
	return f.table, nil
}

const testSecret = "test-secret"

func newTestServiceProxy(t *testing.T, trusted []config.ProxyTrustedServiceConfig) *ServiceProxy {
	t.Helper()
	rp, err := NewServiceProxy(nil, trusted, testSecret, nil, zap.NewNop())
	if err != nil {
		t.Fatal(err)
	}
	return rp
}

func testToken(t *testing.T, roles []string) string {
	t.Helper()
	token, err := sharedjwt.Generate(testSecret, 42, "alice", roles, 1)
	if err != nil {
		t.Fatal(err)
	}
	return token
}
