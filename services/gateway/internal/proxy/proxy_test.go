package proxy

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
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
	rp.SetAdminChecker(func(ctx context.Context, authHeader string, userID int64) (bool, error) {
		if authHeader == "" {
			t.Fatalf("admin checker should receive Authorization header")
		}
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

func TestServiceProxyUsesRouteTableUpstreamWithoutStaticTrustedService(t *testing.T) {
	var gotPath string
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		_, _ = w.Write([]byte("dynamic-upstream"))
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	rp.SetRouteTable(servicestatus.RouteTable{
		Routes: []servicestatus.ServiceRoute{{
			RouteID:      "demo:/api/demo",
			Prefix:       "/api/demo",
			ServiceID:    "demo-api",
			UpstreamBase: upstream.URL,
			AuthMode:     "public",
			Enabled:      true,
			ProxyEnabled: true,
			Status:       "active",
			StripPrefix:  "/api",
		}},
		CanProxy: true,
	})

	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/api/demo/ping", nil))
	if rr.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d body=%s", rr.Code, rr.Body.String())
	}
	if rr.Body.String() != "dynamic-upstream" {
		t.Fatalf("expected dynamic upstream response, got %q", rr.Body.String())
	}
	if gotPath != "/demo/ping" {
		t.Fatalf("unexpected upstream path: %s", gotPath)
	}
}

func TestServiceProxyChecksDynamicRoutePermission(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	rp.SetPermissionChecker(func(ctx context.Context, authHeader string, caller PermissionCheckCaller, permission string) (bool, error) {
		if authHeader == "" {
			t.Fatalf("permission checker should receive Authorization header")
		}
		return caller.Type == "user" && caller.UserID == 42 && permission == "demo.read", nil
	})
	rp.SetRouteTable(servicestatus.RouteTable{
		Routes: []servicestatus.ServiceRoute{{
			RouteID:            "demo:/api/demo",
			Prefix:             "/api/demo",
			ServiceID:          "demo-api",
			UpstreamBase:       upstream.URL,
			AuthMode:           "public",
			RequiredPermission: "demo.read",
			Enabled:            true,
			ProxyEnabled:       true,
			Status:             "active",
		}},
		CanProxy: true,
	})

	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/api/demo/ping", nil))
	if rr.Code != http.StatusUnauthorized {
		t.Fatalf("permission-protected route should require token, got %d body=%s", rr.Code, rr.Body.String())
	}

	rr = httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/api/demo/ping", nil)
	req.Header.Set("Authorization", "Bearer "+testToken(t, []string{"user"}))
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("permission checker should allow route, got %d body=%s", rr.Code, rr.Body.String())
	}

	rp.SetPermissionChecker(func(ctx context.Context, authHeader string, caller PermissionCheckCaller, permission string) (bool, error) {
		if authHeader == "" {
			t.Fatalf("permission checker should receive Authorization header")
		}
		return false, nil
	})
	rr = httptest.NewRecorder()
	req = httptest.NewRequest(http.MethodGet, "/api/demo/ping", nil)
	req.Header.Set("Authorization", "Bearer "+testToken(t, []string{"user"}))
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusForbidden {
		t.Fatalf("permission checker should reject route, got %d body=%s", rr.Code, rr.Body.String())
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

func TestServiceProxyInternalAPIResolverCallsAncestorStorageByAPIID(t *testing.T) {
	objects := map[string][]byte{}
	headers := map[string]http.Header{}
	var mu sync.Mutex
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !strings.HasPrefix(r.URL.Path, "/api/storage/objects/submissions/") {
			t.Fatalf("unexpected storage path %s", r.URL.Path)
		}
		key := strings.TrimPrefix(r.URL.Path, "/api/storage/objects/submissions/")
		mu.Lock()
		headers[r.Method+" "+key] = r.Header.Clone()
		mu.Unlock()
		switch r.Method {
		case http.MethodPut:
			body, err := io.ReadAll(r.Body)
			if err != nil {
				t.Fatal(err)
			}
			mu.Lock()
			objects[key] = body
			mu.Unlock()
			sum := sha256.Sum256(body)
			_ = json.NewEncoder(w).Encode(map[string]any{
				"bucket":       "submissions",
				"key":          key,
				"size_bytes":   len(body),
				"sha256":       hex.EncodeToString(sum[:]),
				"content_type": r.Header.Get("Content-Type"),
			})
		case http.MethodGet:
			mu.Lock()
			body, ok := objects[key]
			mu.Unlock()
			if !ok {
				http.NotFound(w, r)
				return
			}
			_, _ = w.Write(body)
		default:
			t.Fatalf("unexpected method %s", r.Method)
		}
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	rp.SetRouteTable(servicestatus.RouteTable{
		Routes: []servicestatus.ServiceRoute{
			ancestorStorageRoute("storage.object.put", http.MethodPut, upstream.URL, true),
			ancestorStorageRoute("storage.object.get", http.MethodGet, upstream.URL, true),
		},
		CanProxy: true,
	})

	putReq := httptest.NewRequest(
		http.MethodPut,
		"/internal/apis/storage.object.put/submissions/42-source-main.cpp",
		bytes.NewBufferString("int main(){}"),
	)
	putReq.Header.Set("X-OJOS-Node-Id", "child-node")
	putReq.Header.Set("Content-Type", "text/plain; charset=utf-8")
	putResp := httptest.NewRecorder()
	rp.ServeHTTP(putResp, putReq)
	if putResp.Code != http.StatusOK {
		t.Fatalf("expected put 200, got %d body=%s", putResp.Code, putResp.Body.String())
	}

	getReq := httptest.NewRequest(
		http.MethodGet,
		"/internal/apis/storage.object.get/submissions/42-source-main.cpp",
		nil,
	)
	getReq.Header.Set("X-OJOS-Node-Id", "child-node")
	getResp := httptest.NewRecorder()
	rp.ServeHTTP(getResp, getReq)
	if getResp.Code != http.StatusOK {
		t.Fatalf("expected get 200, got %d body=%s", getResp.Code, getResp.Body.String())
	}
	if getResp.Body.String() != "int main(){}" {
		t.Fatalf("unexpected storage body %q", getResp.Body.String())
	}

	gotHeaders := headers["PUT 42-source-main.cpp"]
	if gotHeaders.Get("X-OJOS-Api-Id") != "storage.object.put" ||
		gotHeaders.Get("X-OJOS-Caller-Node-Id") != "child-node" ||
		gotHeaders.Get("X-OJOS-Provider-Node-Id") != "root-node" ||
		gotHeaders.Get("X-OJOS-Provider-Service") != "storage-service" ||
		gotHeaders.Get("X-OJOS-Provider-Endpoint") != "127.0.0.1:8085:storage-service" {
		t.Fatalf("missing resolver trace headers: %#v", gotHeaders)
	}
	if gotHeaders.Get("X-OJOS-Resolved-Provider-Endpoint") != "" {
		t.Fatalf("internal resolver metadata leaked upstream")
	}
}

func TestServiceProxyInternalAPIResolverRejectsUnavailableAndInvisibleRoutes(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	rp.SetNodeID("child-node")
	rp.SetRouteTable(servicestatus.RouteTable{
		Routes: []servicestatus.ServiceRoute{
			ancestorStorageRoute("storage.object.get", http.MethodGet, upstream.URL, false),
		},
	})

	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/internal/apis/storage.sibling.get/submissions/a.cpp", nil))
	if rr.Code != http.StatusNotFound {
		t.Fatalf("sibling/non-visible api should be 404, got %d body=%s", rr.Code, rr.Body.String())
	}

	rr = httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/internal/apis/storage.private.admin/submissions/a.cpp", nil))
	if rr.Code != http.StatusNotFound {
		t.Fatalf("private/non-visible api should be 404, got %d body=%s", rr.Code, rr.Body.String())
	}

	rr = httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil))
	if rr.Code != http.StatusServiceUnavailable {
		t.Fatalf("stopped provider should be 503, got %d body=%s", rr.Code, rr.Body.String())
	}
}

func TestServiceProxyInternalAPIResolverChecksPermissions(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	route := ancestorStorageRoute("storage.object.get", http.MethodGet, upstream.URL, true)
	route.AuthMode = "user"
	route.RequiredPermission = "storage.object.read"
	rp.SetRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{route}, CanProxy: true})
	rp.SetPermissionChecker(func(ctx context.Context, authHeader string, caller PermissionCheckCaller, permission string) (bool, error) {
		return false, nil
	})

	req := httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil)
	req.Header.Set("X-OJOS-Node-Id", "child-node")
	req.Header.Set("Authorization", "Bearer "+testToken(t, []string{"user"}))
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusForbidden {
		t.Fatalf("permission denied should be 403, got %d body=%s", rr.Code, rr.Body.String())
	}
}

func TestServiceProxyInternalAPIResolverUsesServiceCallerIdentity(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("X-OJOS-Caller-Service") != "judge-worker" {
			t.Fatalf("caller service header should be forwarded to provider: %#v", r.Header)
		}
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	route := ancestorStorageRoute("storage.object.get", http.MethodGet, upstream.URL, true)
	route.AuthMode = "service"
	route.RequiredPermission = "storage.object.read"
	rp.SetRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{route}, CanProxy: true})
	rp.SetPermissionChecker(func(ctx context.Context, authHeader string, caller PermissionCheckCaller, permission string) (bool, error) {
		if authHeader != "Bearer internal-token" {
			t.Fatalf("service permission check should receive bearer token, got %q", authHeader)
		}
		if caller.Type != "service" ||
			caller.Service != "judge-worker" ||
			caller.NodeID != "child-node" ||
			caller.APIID != "storage.object.get" ||
			permission != "storage.object.read" {
			t.Fatalf("unexpected service caller permission request: caller=%#v permission=%s", caller, permission)
		}
		return true, nil
	})

	missing := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil)
	req.Header.Set("X-OJOS-Node-Id", "child-node")
	rp.ServeHTTP(missing, req)
	if missing.Code != http.StatusUnauthorized {
		t.Fatalf("missing service token should be 401, got %d body=%s", missing.Code, missing.Body.String())
	}

	req = httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil)
	req.Header.Set("X-OJOS-Node-Id", "child-node")
	req.Header.Set("Authorization", "Bearer internal-token")
	missingService := httptest.NewRecorder()
	rp.ServeHTTP(missingService, req)
	if missingService.Code != http.StatusUnauthorized {
		t.Fatalf("missing caller service should be 401, got %d body=%s", missingService.Code, missingService.Body.String())
	}

	req = httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil)
	req.Header.Set("X-OJOS-Node-Id", "child-node")
	req.Header.Set("X-OJOS-Caller-Service", "judge-worker")
	req.Header.Set("Authorization", "Bearer internal-token")
	allowed := httptest.NewRecorder()
	rp.ServeHTTP(allowed, req)
	if allowed.Code != http.StatusNoContent {
		t.Fatalf("service caller should be allowed, got %d body=%s", allowed.Code, allowed.Body.String())
	}
}

func TestServiceProxyInternalAPIResolverAllowsPublicWithoutToken(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	route := ancestorStorageRoute("storage.object.get", http.MethodGet, upstream.URL, true)
	route.AuthMode = "public"
	route.RequiredPermission = "public"
	rp.SetRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{route}, CanProxy: true})

	req := httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil)
	req.Header.Set("X-OJOS-Node-Id", "child-node")
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("public api should not need token, got %d body=%s", rr.Code, rr.Body.String())
	}
}

func TestServiceProxyReloadMakesNewInternalAPIAvailable(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	rp.SetNodeID("child-node")

	req := httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil)
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusNotFound {
		t.Fatalf("api should be unavailable before reload, got %d", rr.Code)
	}

	_, err := rp.Reload(context.Background(), fakeServiceRouteReader{table: servicestatus.RouteTable{
		Routes: []servicestatus.ServiceRoute{
			ancestorStorageRoute("storage.object.get", http.MethodGet, upstream.URL, true),
		},
		CanProxy: true,
	}})
	if err != nil {
		t.Fatal(err)
	}

	req = httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil)
	rr = httptest.NewRecorder()
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("api should be available after reload, got %d body=%s", rr.Code, rr.Body.String())
	}
}

func ancestorStorageRoute(apiID string, method string, upstream string, running bool) servicestatus.ServiceRoute {
	status := "active"
	serviceStatus := servicestatus.ServiceStatusRunning
	proxyEnabled := true
	var blocked []string
	if !running {
		status = "unavailable"
		serviceStatus = servicestatus.ServiceStatusStopped
		proxyEnabled = false
		blocked = []string{"service not running"}
	}
	return servicestatus.ServiceRoute{
		RouteID:            "storage-service:" + apiID,
		ApiID:              apiID,
		NodeID:             "child-node",
		ProviderNodeID:     "root-node",
		ProviderHostIP:     "127.0.0.1",
		ProviderService:    "storage-service",
		ProviderEndpoint:   "127.0.0.1:8085:storage-service",
		VisibilitySource:   "ancestor-descendants",
		Distance:           1,
		OwnerServiceID:     "storage-service",
		Prefix:             "/api/storage/objects",
		ServiceID:          "storage-service",
		TargetService:      "storage-service",
		UpstreamBase:       upstream,
		AuthMode:           "public",
		RequiredPermission: "public",
		Methods:            []string{method},
		Enabled:            true,
		ProxyEnabled:       proxyEnabled,
		Priority:           len("/api/storage/objects"),
		CreatedFrom:        "orchestrator_effective_api_view",
		Status:             status,
		ServiceStatus:      serviceStatus,
		BlockedBy:          blocked,
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
