package proxy

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/orchestrator/servicestatus"
	sharedjwt "ojos-shared/security/jwt"
	"ojos-shared/security/workload"

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

func TestContributionRoutesMatchMethodAndRewriteTemplateParameters(t *testing.T) {
	var gotMethod string
	var gotPath string
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotMethod, gotPath = r.Method, r.URL.Path
		_, _ = io.WriteString(w, r.Method+" "+r.URL.Path)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	route := func(method, operation, providerPath string) servicestatus.ServiceRoute {
		return servicestatus.ServiceRoute{
			RouteID: "contribution:contest:" + operation, ApiID: "contest.api", OperationID: operation,
			DeploymentID: "dep-1", RevisionID: "rev-1", Generation: 3, Audience: "user",
			PathTemplate: "/api/contests/{contestId}", ProviderPath: providerPath,
			Prefix: "/api/contests/{contestId}", ServiceID: "contest-service", UpstreamBase: upstream.URL,
			AuthMode: "public", Methods: []string{method}, Enabled: true, ProxyEnabled: true, Status: "active",
			CreatedFrom: "contribution_snapshot_v1",
		}
	}
	rp.SetContributionRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{
		route(http.MethodGet, "getContest", "/v1/contests/{contestId}"),
		route(http.MethodPut, "updateContest", "/v1/contests/{contestId}"),
	}, CanProxy: true})

	for _, method := range []string{http.MethodGet, http.MethodPut} {
		rr := httptest.NewRecorder()
		rp.ServeHTTP(rr, httptest.NewRequest(method, "/api/contests/contest-42?expand=owner", nil))
		if rr.Code != http.StatusOK || gotMethod != method || gotPath != "/v1/contests/contest-42" {
			t.Fatalf("%s route mismatch: status=%d upstream=%s %s body=%s", method, rr.Code, gotMethod, gotPath, rr.Body.String())
		}
	}

	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodDelete, "/api/contests/contest-42", nil))
	if rr.Code != http.StatusNotFound {
		t.Fatalf("unpublished method must not match contribution template, got %d", rr.Code)
	}
	rr = httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/api/contests/a/b", nil))
	if rr.Code != http.StatusNotFound {
		t.Fatalf("template parameter must consume exactly one path segment, got %d", rr.Code)
	}
	rr = httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/api/contests/%2Fadmin", nil))
	if rr.Code != http.StatusNotFound {
		t.Fatalf("encoded slash must not be smuggled through provider rewrite, got %d", rr.Code)
	}
}

func TestContributionRoutesIsolateAudience(t *testing.T) {
	userUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, "user")
	}))
	defer userUpstream.Close()
	adminUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get(contributionAudienceHeader) != "" {
			t.Fatal("caller-controlled audience header leaked upstream")
		}
		_, _ = io.WriteString(w, "admin")
	}))
	defer adminUpstream.Close()

	route := func(audience, revision, upstream string) servicestatus.ServiceRoute {
		return servicestatus.ServiceRoute{
			RouteID: "contribution:contest:" + audience, ApiID: "contest.api", OperationID: "listContest",
			DeploymentID: "dep-1", RevisionID: revision, Generation: 1, Audience: audience,
			PathTemplate: "/api/shared", ProviderPath: "/shared", Prefix: "/api/shared",
			ServiceID: "contest-service", UpstreamBase: upstream, AuthMode: "public", Methods: []string{"GET"},
			Enabled: true, ProxyEnabled: true, Status: "active", CreatedFrom: "contribution_snapshot_v1",
		}
	}
	rp := newTestServiceProxy(t, nil)
	rp.SetContributionRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{
		route("user", "rev-user", userUpstream.URL), route("admin", "rev-admin", adminUpstream.URL),
	}, CanProxy: true})

	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/api/shared", nil))
	if rr.Code != http.StatusOK || rr.Body.String() != "user" {
		t.Fatalf("default user audience selected wrong route: status=%d body=%q", rr.Code, rr.Body.String())
	}
	req := httptest.NewRequest(http.MethodGet, "/api/shared", nil)
	req.Header.Set(contributionAudienceHeader, "admin")
	rr = httptest.NewRecorder()
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusOK || rr.Body.String() != "admin" {
		t.Fatalf("admin audience selected wrong route: status=%d body=%q", rr.Code, rr.Body.String())
	}
}

func TestContributionRouteExecutesOperationPermission(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	rp.SetPermissionChecker(func(_ context.Context, _ string, caller PermissionCheckCaller, permission string) (bool, error) {
		return caller.UserID == 42 && caller.APIID == "contest.api" && permission == "contest.read", nil
	})
	rp.SetContributionRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{{
		RouteID: "contribution:contest:getContest", ApiID: "contest.api", OperationID: "getContest",
		DeploymentID: "dep-1", RevisionID: "rev-1", Generation: 1, Audience: "user",
		PathTemplate: "/api/contests/{contestId}", ProviderPath: "/contests/{contestId}",
		Prefix: "/api/contests/{contestId}", ServiceID: "contest-service", UpstreamBase: upstream.URL,
		AuthMode: "user", RequiredPermission: "contest.read", Methods: []string{"GET"},
		Enabled: true, ProxyEnabled: true, Status: "active", CreatedFrom: "contribution_snapshot_v1",
	}}, CanProxy: true})

	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/api/contests/42", nil))
	if rr.Code != http.StatusUnauthorized {
		t.Fatalf("operation permission must require a user, got %d", rr.Code)
	}
	req := httptest.NewRequest(http.MethodGet, "/api/contests/42", nil)
	req.Header.Set("Authorization", "Bearer "+testToken(t, []string{"user"}))
	rr = httptest.NewRecorder()
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("operation permission should allow verified user, got %d body=%s", rr.Code, rr.Body.String())
	}
}

func TestContributionRouteDerivesPermissionScopeFromMatchedPathOnly(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(http.StatusNoContent) }))
	defer upstream.Close()
	rp := newTestServiceProxy(t, nil)
	rp.SetPermissionChecker(func(_ context.Context, _ string, caller PermissionCheckCaller, permission string) (bool, error) {
		return caller.UserID == 42 && caller.ScopeType == "contest" && caller.ScopeID == 73 && permission == "contest.read", nil
	})
	rp.SetContributionRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{{
		RouteID: "contribution:contest:getContest", ApiID: "contest.api", OperationID: "getContest", DeploymentID: "dep-1", RevisionID: "rev-1", Generation: 1, Audience: "user",
		PathTemplate: "/api/contests/{contestId}", ProviderPath: "/contests/{contestId}", Prefix: "/api/contests/{contestId}", ServiceID: "contest-service", UpstreamBase: upstream.URL,
		AuthMode: "user", RequiredPermission: "contest.read", PermissionScope: &servicestatus.PermissionScope{Kind: "path_parameter", Type: "contest", PathParameter: "contestId"},
		Methods: []string{"GET"}, Enabled: true, ProxyEnabled: true, Status: "active", CreatedFrom: "contribution_snapshot_v1",
	}}, CanProxy: true})
	req := httptest.NewRequest(http.MethodGet, "/api/contests/73?contestId=999", nil)
	req.Header.Set("Authorization", "Bearer "+testToken(t, []string{"user"}))
	req.Header.Set("X-OJOS-Scope-Id", "999")
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("path-derived scope should be authorized, got %d body=%s", rr.Code, rr.Body.String())
	}
	req = httptest.NewRequest(http.MethodGet, "/api/contests/073", nil)
	req.Header.Set("Authorization", "Bearer "+testToken(t, []string{"user"}))
	rr = httptest.NewRecorder()
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusBadRequest {
		t.Fatalf("non-canonical resource id must fail closed, got %d body=%s", rr.Code, rr.Body.String())
	}
}

func TestInvalidContributionRevisionPreservesActiveRouteTable(t *testing.T) {
	oldUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, "old")
	}))
	defer oldUpstream.Close()
	rp := newTestServiceProxy(t, nil)
	valid := servicestatus.ServiceRoute{
		RouteID: "contribution:contest:old", ApiID: "contest.api", OperationID: "getContest",
		DeploymentID: "dep-1", RevisionID: "rev-old", Generation: 1, Audience: "user",
		PathTemplate: "/api/contests/{contestId}", ProviderPath: "/contests/{contestId}",
		Prefix: "/api/contests/{contestId}", ServiceID: "contest-service", UpstreamBase: oldUpstream.URL,
		AuthMode: "public", Methods: []string{"GET"}, Enabled: true, ProxyEnabled: true,
		Status: "active", CreatedFrom: "contribution_snapshot_v1",
	}
	if err := rp.TrySetContributionRouteTable(servicestatus.RouteTable{Version: "old", Routes: []servicestatus.ServiceRoute{valid}, CanProxy: true}); err != nil {
		t.Fatal(err)
	}
	invalid := valid
	invalid.RouteID, invalid.RevisionID, invalid.UpstreamBase = "contribution:contest:new", "rev-new", "file:///tmp/not-an-upstream"
	if err := rp.TrySetContributionRouteTable(servicestatus.RouteTable{Version: "new", Routes: []servicestatus.ServiceRoute{invalid}, CanProxy: true}); err == nil {
		t.Fatal("invalid candidate contribution revision was published")
	}
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/api/contests/42", nil))
	if rr.Code != http.StatusOK || rr.Body.String() != "old" {
		t.Fatalf("invalid candidate evicted active revision: status=%d body=%q", rr.Code, rr.Body.String())
	}
}

func TestContributionRevisionSwitchKeepsOldInFlightSnapshot(t *testing.T) {
	entered := make(chan struct{})
	release := make(chan struct{})
	oldUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		close(entered)
		<-release
		_, _ = io.WriteString(w, "old")
	}))
	defer oldUpstream.Close()
	newUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, "new")
	}))
	defer newUpstream.Close()

	route := func(revision, upstream string) servicestatus.ServiceRoute {
		return servicestatus.ServiceRoute{
			RouteID: "contribution:contest:" + revision, ApiID: "contest.api", OperationID: "getContest",
			DeploymentID: "dep-1", RevisionID: revision, Generation: 1, Audience: "user",
			PathTemplate: "/api/contests/{contestId}", ProviderPath: "/contests/{contestId}",
			Prefix: "/api/contests/{contestId}", ServiceID: "contest-service", UpstreamBase: upstream,
			AuthMode: "public", Methods: []string{"GET"}, Enabled: true, ProxyEnabled: true,
			Status: "active", CreatedFrom: "contribution_snapshot_v1",
		}
	}
	rp := newTestServiceProxy(t, nil)
	if err := rp.TrySetContributionRouteTable(servicestatus.RouteTable{Version: "old", Routes: []servicestatus.ServiceRoute{route("old", oldUpstream.URL)}, CanProxy: true}); err != nil {
		t.Fatal(err)
	}
	type result struct {
		status int
		body   string
	}
	oldResult := make(chan result, 1)
	go func() {
		rr := httptest.NewRecorder()
		rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/api/contests/1", nil))
		oldResult <- result{status: rr.Code, body: rr.Body.String()}
	}()
	<-entered
	if err := rp.TrySetContributionRouteTable(servicestatus.RouteTable{Version: "new", Routes: []servicestatus.ServiceRoute{route("new", newUpstream.URL)}, CanProxy: true}); err != nil {
		t.Fatal(err)
	}
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/api/contests/2", nil))
	if rr.Code != http.StatusOK || rr.Body.String() != "new" {
		t.Fatalf("new request did not use new revision: status=%d body=%q", rr.Code, rr.Body.String())
	}
	close(release)
	old := <-oldResult
	if old.status != http.StatusOK || old.body != "old" {
		t.Fatalf("in-flight request lost old snapshot: %#v", old)
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
		if got := r.Header.Get("Authorization"); got != "" {
			t.Fatalf("service credential must be stripped before unrelated provider, got %q", got)
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

func TestServiceProxyWorkloadBindingUsesDeploymentJWTAndSanitizesCallerHeaders(t *testing.T) {
	var upstreamHeaders http.Header
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		upstreamHeaders = r.Header.Clone()
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	issuer, err := workload.NewIssuer(privateKey, "workload-1", "issuer", "gateway", 15*time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	verifier, err := workload.NewVerifier(publicKey, "workload-1", "issuer", "gateway")
	if err != nil {
		t.Fatal(err)
	}
	token, _, err := issuer.Issue(workload.IssueRequest{
		DeploymentID:         "deployment-worker-b",
		ServiceID:            "judge-worker",
		NodeID:               "node-b",
		CredentialGeneration: 3,
	}, time.Now())
	if err != nil {
		t.Fatal(err)
	}

	rp := newTestServiceProxy(t, nil)
	rp.SetWorkloadVerifier(verifier)
	route := ancestorStorageRoute("storage.object.get", http.MethodGet, upstream.URL, true)
	route.AuthMode = "workload"
	route.ProviderAuthMode = "workload"
	route.RequiredPermission = "storage.object.read"
	route.BindingID = "binding-storage-get"
	route.ConsumerDeploymentID = "deployment-worker-b"
	route.ConsumerServiceID = "judge-worker"
	route.ConsumerNodeID = "node-b"
	route.CredentialGeneration = 3
	rp.SetRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{route}, CanProxy: true})

	req := httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil)
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("X-OJOS-Caller-Service", "forged-service")
	req.Header.Set("X-OJOS-Node-Id", "forged-node")
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("expected workload request to succeed, got %d body=%s", rr.Code, rr.Body.String())
	}
	if got := upstreamHeaders.Get("X-OJOS-Caller-Service"); got != "judge-worker" {
		t.Fatalf("caller service was not rebuilt from JWT: %q", got)
	}
	if got := upstreamHeaders.Get("X-OJOS-Caller-Node-Id"); got != "node-b" {
		t.Fatalf("caller node was not rebuilt from JWT: %q", got)
	}
	if got := upstreamHeaders.Get("X-OJOS-Caller-Deployment-Id"); got != "deployment-worker-b" {
		t.Fatalf("caller deployment was not forwarded: %q", got)
	}
	if got := upstreamHeaders.Get("X-OJOS-Binding-Id"); got != "binding-storage-get" {
		t.Fatalf("binding id was not forwarded: %q", got)
	}
	if got := upstreamHeaders.Get("Authorization"); got != "Bearer "+token {
		t.Fatal("workload assertion must be forwarded to the bound provider")
	}

	for name, identity := range map[string]workload.IssueRequest{
		"wrong deployment": {
			DeploymentID: "deployment-worker-c", ServiceID: "judge-worker", NodeID: "node-b", CredentialGeneration: 3,
		},
		"wrong service": {
			DeploymentID: "deployment-worker-b", ServiceID: "other-worker", NodeID: "node-b", CredentialGeneration: 3,
		},
		"wrong node": {
			DeploymentID: "deployment-worker-b", ServiceID: "judge-worker", NodeID: "node-c", CredentialGeneration: 3,
		},
		"stale generation": {
			DeploymentID: "deployment-worker-b", ServiceID: "judge-worker", NodeID: "node-b", CredentialGeneration: 2,
		},
	} {
		t.Run(name, func(t *testing.T) {
			mismatched, _, err := issuer.Issue(identity, time.Now())
			if err != nil {
				t.Fatal(err)
			}
			req := httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil)
			req.Header.Set("Authorization", "Bearer "+mismatched)
			rr := httptest.NewRecorder()
			rp.ServeHTTP(rr, req)
			if rr.Code != http.StatusForbidden {
				t.Fatalf("mismatched assignment should be rejected, got %d body=%s", rr.Code, rr.Body.String())
			}
		})
	}

	// Consumer authentication remains workload-scoped even when the selected
	// upstream API is public. The provider must not receive the reusable JWT.
	upstreamHeaders = nil
	route.ProviderAuthMode = "public"
	rp.SetRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{route}, CanProxy: true})
	req = httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get/submissions/a.cpp", nil)
	req.Header.Set("Authorization", "Bearer "+token)
	rr = httptest.NewRecorder()
	rp.ServeHTTP(rr, req)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("public provider binding should accept authenticated consumer, got %d body=%s", rr.Code, rr.Body.String())
	}
	if got := upstreamHeaders.Get("Authorization"); got != "" {
		t.Fatalf("public provider must not receive workload bearer, got %q", got)
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

func TestServiceProxyInternalAPIWithoutTailKeepsServiceCallerCredential(t *testing.T) {
	var gotPath string
	var gotAuth string
	var gotBody string

	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		gotAuth = r.Header.Get("Authorization")
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Fatal(err)
		}
		gotBody = string(body)
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	route := ancestorStorageRoute("auth.user.permission.check", http.MethodPost, upstream.URL, true)
	route.Prefix = "/auth/admin/permission-check"
	route.ProviderService = "auth-service"
	route.ProviderEndpoint = "127.0.0.1:8081:auth-service"
	route.OwnerServiceID = "auth-service"
	route.ServiceID = "auth-service"
	route.TargetService = "auth-service"
	route.AuthMode = "service"
	route.RequiredPermission = "auth.permission.check"
	rp.SetRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{route}, CanProxy: true})
	rp.SetPermissionChecker(func(ctx context.Context, authHeader string, caller PermissionCheckCaller, permission string) (bool, error) {
		return true, nil
	})

	req := httptest.NewRequest(
		http.MethodPost,
		"/internal/apis/auth.user.permission.check",
		bytes.NewBufferString(`{"user_id":42,"permission":"judge.submit"}`),
	)
	req.Header.Set("X-OJOS-Node-Id", "child-node")
	req.Header.Set("X-OJOS-Caller-Service", "user-service")
	req.Header.Set("Authorization", "Bearer service-token")
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, req)

	if rr.Code != http.StatusNoContent {
		t.Fatalf("expected 204, got %d body=%s", rr.Code, rr.Body.String())
	}
	if gotPath != "/auth/admin/permission-check" {
		t.Fatalf("api_id call without tail must not gain a trailing slash, got %q", gotPath)
	}
	if gotAuth != "Bearer service-token" {
		t.Fatalf("service caller credential must reach the provider, got %q", gotAuth)
	}
	if gotBody != `{"user_id":42,"permission":"judge.submit"}` {
		t.Fatalf("unexpected forwarded body %q", gotBody)
	}
}

func TestServiceProxyPermissionAPINonAuthProviderDropsServiceCredential(t *testing.T) {
	var gotAuth string
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotAuth = r.Header.Get("Authorization")
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	route := ancestorStorageRoute("auth.user.permission.check", http.MethodPost, upstream.URL, true)
	route.Prefix = "/auth/admin/permission-check"
	route.AuthMode = "service"
	route.RequiredPermission = "auth.permission.check"
	rp.SetRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{route}, CanProxy: true})
	rp.SetPermissionChecker(func(ctx context.Context, authHeader string, caller PermissionCheckCaller, permission string) (bool, error) {
		return true, nil
	})

	req := httptest.NewRequest(
		http.MethodPost,
		"/internal/apis/auth.user.permission.check",
		bytes.NewBufferString(`{"user_id":42,"permission":"judge.submit"}`),
	)
	req.Header.Set("X-OJOS-Node-Id", "child-node")
	req.Header.Set("X-OJOS-Caller-Service", "user-service")
	req.Header.Set("Authorization", "Bearer service-token")
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, req)

	if rr.Code != http.StatusNoContent {
		t.Fatalf("expected 204, got %d body=%s", rr.Code, rr.Body.String())
	}
	if gotAuth != "" {
		t.Fatalf("non-auth-service provider must not receive the service credential, got %q", gotAuth)
	}
}

func TestServiceProxyRejectsPublicPermissionForServiceAuth(t *testing.T) {
	upstreamCalled := false
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		upstreamCalled = true
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	route := ancestorStorageRoute("storage.object.get", http.MethodGet, upstream.URL, true)
	route.AuthMode = "service"
	route.RequiredPermission = "public"
	rp.SetRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{route}, CanProxy: true})
	rp.SetPermissionChecker(func(ctx context.Context, authHeader string, caller PermissionCheckCaller, permission string) (bool, error) {
		t.Fatal("invalid service/public route must fail before permission checking")
		return true, nil
	})

	req := httptest.NewRequest(http.MethodGet, "/internal/apis/storage.object.get", nil)
	req.Header.Set("X-OJOS-Node-Id", "child-node")
	req.Header.Set("X-OJOS-Caller-Service", "forged-service")
	req.Header.Set("Authorization", "Bearer forged-token")
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, req)

	if rr.Code != http.StatusInternalServerError {
		t.Fatalf("expected fail-closed 500, got %d body=%s", rr.Code, rr.Body.String())
	}
	if upstreamCalled {
		t.Fatal("invalid service/public route must not reach the provider")
	}
}

func TestServiceProxyPreservesKnownLengthObjectStream(t *testing.T) {
	payload := bytes.Repeat([]byte("package-data\n"), 96*1024)
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/zip")
		w.Header().Set("Content-Length", strconv.Itoa(len(payload)))
		_, _ = w.Write(payload)
	}))
	defer upstream.Close()

	rp, err := NewServiceProxy([]config.ProxyRouteConfig{{
		Prefix:    "/api/files",
		Target:    upstream.URL,
		AuthMode:  "public",
		TimeoutMS: 5000,
	}}, nil, testSecret, nil, zap.NewNop())
	if err != nil {
		t.Fatal(err)
	}
	gateway := httptest.NewServer(rp)
	defer gateway.Close()

	resp, err := http.Get(gateway.URL + "/api/files/problem.zip")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("read proxied object: %v", err)
	}
	if !bytes.Equal(body, payload) {
		t.Fatalf("proxied object size = %d, want %d", len(body), len(payload))
	}
	if resp.ContentLength != int64(len(payload)) {
		t.Fatalf("proxied Content-Length = %d, want %d", resp.ContentLength, len(payload))
	}
	if len(resp.TransferEncoding) != 0 {
		t.Fatalf("known-length object became transfer encoded: %#v", resp.TransferEncoding)
	}
}

func TestServiceProxyEnforcesTotalRouteTimeoutAfterHeaders(t *testing.T) {
	upstreamCancelled := make(chan struct{})
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		defer close(upstreamCancelled)
		w.Header().Set("Content-Length", "2")
		_, _ = w.Write([]byte("a"))
		w.(http.Flusher).Flush()
		<-r.Context().Done()
	}))
	defer upstream.Close()

	rp, err := NewServiceProxy([]config.ProxyRouteConfig{{
		Prefix:    "/api/slow",
		Target:    upstream.URL,
		AuthMode:  "public",
		TimeoutMS: 1000,
	}}, nil, testSecret, nil, zap.NewNop())
	if err != nil {
		t.Fatal(err)
	}
	gateway := httptest.NewServer(rp)
	defer gateway.Close()

	started := time.Now()
	resp, err := http.Get(gateway.URL + "/api/slow/claim")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	first := make([]byte, 1)
	if _, err := io.ReadFull(resp.Body, first); err != nil || string(first) != "a" {
		t.Fatalf("first streamed byte = %q, err=%v", first, err)
	}
	if time.Since(started) >= time.Second {
		t.Fatal("Gateway buffered the first byte until the route deadline")
	}
	if _, err := io.ReadAll(resp.Body); err == nil {
		t.Fatal("route body that exceeded its total timeout completed successfully")
	}
	if elapsed := time.Since(started); elapsed < 900*time.Millisecond || elapsed > 3*time.Second {
		t.Fatalf("route timeout fired after %s, want about 1s", elapsed)
	}
	select {
	case <-upstreamCancelled:
	case <-time.After(time.Second):
		t.Fatal("route deadline did not cancel the upstream request")
	}
}

func TestRouteTotalTimeoutClampsUntrustedValues(t *testing.T) {
	if got := routeTotalTimeout(1, 0); got != time.Second {
		t.Fatalf("minimum route timeout = %s", got)
	}
	if got := routeTotalTimeout(^uint64(0), 0); got != 10*time.Minute {
		t.Fatalf("maximum route timeout = %s", got)
	}
	if got := routeTotalTimeout(0, 35*time.Second); got != 35*time.Second {
		t.Fatalf("fallback route timeout = %s", got)
	}
}

func TestCompiledRouteReusesProxyAndTransportIdentity(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	route := ancestorStorageRoute("storage.object.get", http.MethodGet, upstream.URL, true)
	route.Prefix = "/api/reuse"
	rp.SetRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{route}, CanProxy: true})

	snapshot := rp.acquireRouteTable()
	if snapshot == nil {
		t.Fatal("compiled route table is unavailable")
	}
	defer snapshot.release()
	first, ok := rp.matchServiceRoute(snapshot, "/api/reuse/object")
	if !ok {
		t.Fatal("first route lookup failed")
	}
	second, ok := rp.matchServiceRoute(snapshot, "/api/reuse/object")
	if !ok {
		t.Fatal("second route lookup failed")
	}
	if first.proxy != second.proxy {
		t.Fatal("route lookup rebuilt ReverseProxy")
	}
	if first.proxy.Transport != second.proxy.Transport {
		t.Fatal("route lookup rebuilt Transport")
	}
	transport, ok := first.proxy.Transport.(*http.Transport)
	if !ok || transport.IdleConnTimeout <= 0 {
		t.Fatalf("compiled transport has no finite idle timeout: %#v", first.proxy.Transport)
	}
}

func TestCompiledRouteTransportReusesUpstreamConnection(t *testing.T) {
	var newConnections atomic.Int64
	upstream := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, "reused")
	}))
	upstream.Config.ConnState = func(_ net.Conn, state http.ConnState) {
		if state == http.StateNew {
			newConnections.Add(1)
		}
	}
	upstream.Start()
	defer upstream.Close()

	rp := newTestServiceProxy(t, nil)
	route := ancestorStorageRoute("storage.object.get", http.MethodGet, upstream.URL, true)
	route.Prefix = "/api/reuse"
	rp.SetRouteTable(servicestatus.RouteTable{Routes: []servicestatus.ServiceRoute{route}, CanProxy: true})
	gateway := httptest.NewServer(rp)
	defer gateway.Close()

	for i := 0; i < 64; i++ {
		resp, err := http.Get(gateway.URL + "/api/reuse/object")
		if err != nil {
			t.Fatal(err)
		}
		body, readErr := io.ReadAll(resp.Body)
		_ = resp.Body.Close()
		if readErr != nil || string(body) != "reused" {
			t.Fatalf("request %d: body=%q err=%v", i, body, readErr)
		}
	}
	if got := newConnections.Load(); got > 2 {
		t.Fatalf("64 sequential requests opened %d upstream connections; transport was not reused", got)
	}
}

func TestRouteTableReplacementClosesOldPoolAndUsesNewBinding(t *testing.T) {
	oldClosed := make(chan struct{}, 4)
	oldUpstream := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, "old")
	}))
	oldUpstream.Config.ConnState = func(_ net.Conn, state http.ConnState) {
		if state == http.StateClosed {
			select {
			case oldClosed <- struct{}{}:
			default:
			}
		}
	}
	oldUpstream.Start()
	defer oldUpstream.Close()
	newUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, "new")
	}))
	defer newUpstream.Close()

	rp := newTestServiceProxy(t, nil)
	oldRoute := ancestorStorageRoute("storage.object.get", http.MethodGet, oldUpstream.URL, true)
	oldRoute.Prefix = "/api/switch"
	rp.SetRouteTable(servicestatus.RouteTable{Version: "old", Routes: []servicestatus.ServiceRoute{oldRoute}, CanProxy: true})
	gateway := httptest.NewServer(rp)
	defer gateway.Close()
	if got := proxyResponseBody(t, gateway.URL+"/api/switch/object"); got != "old" {
		t.Fatalf("old binding returned %q", got)
	}
	oldSnapshot := rp.table.Load()
	oldCompiled, ok := rp.matchServiceRoute(oldSnapshot, "/api/switch/object")
	if !ok {
		t.Fatal("old compiled binding is missing")
	}

	newRoute := oldRoute
	newRoute.UpstreamBase = newUpstream.URL
	rp.SetRouteTable(servicestatus.RouteTable{Version: "new", Routes: []servicestatus.ServiceRoute{newRoute}, CanProxy: true})
	newSnapshot := rp.table.Load()
	newCompiled, ok := rp.matchServiceRoute(newSnapshot, "/api/switch/object")
	if !ok {
		t.Fatal("new compiled binding is missing")
	}
	if oldCompiled.proxy.Transport == newCompiled.proxy.Transport {
		t.Fatal("new revision reused the retired route transport")
	}
	if got := proxyResponseBody(t, gateway.URL+"/api/switch/object"); got != "new" {
		t.Fatalf("request after table replacement used stale binding: %q", got)
	}
	select {
	case <-oldClosed:
	case <-time.After(2 * time.Second):
		t.Fatal("retired route table did not close its idle upstream connection")
	}
}

func TestRetiredSnapshotClosesConnectionCreatedByLateInFlightRequest(t *testing.T) {
	oldClosed := make(chan struct{}, 1)
	oldUpstream := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, "old-in-flight")
	}))
	oldUpstream.Config.ConnState = func(_ net.Conn, state http.ConnState) {
		if state == http.StateClosed {
			select {
			case oldClosed <- struct{}{}:
			default:
			}
		}
	}
	oldUpstream.Start()
	defer oldUpstream.Close()
	newUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, "new")
	}))
	defer newUpstream.Close()

	rp := newTestServiceProxy(t, nil)
	oldRoute := ancestorStorageRoute("storage.object.get", http.MethodGet, oldUpstream.URL, true)
	oldRoute.Prefix = "/api/late"
	rp.SetRouteTable(servicestatus.RouteTable{Version: "old", Routes: []servicestatus.ServiceRoute{oldRoute}, CanProxy: true})
	oldSnapshot := rp.acquireRouteTable()
	if oldSnapshot == nil {
		t.Fatal("old snapshot is unavailable")
	}
	compiledOld, ok := rp.matchServiceRoute(oldSnapshot, "/api/late/object")
	if !ok {
		oldSnapshot.release()
		t.Fatal("old route is unavailable")
	}

	newRoute := oldRoute
	newRoute.UpstreamBase = newUpstream.URL
	rp.SetRouteTable(servicestatus.RouteTable{Version: "new", Routes: []servicestatus.ServiceRoute{newRoute}, CanProxy: true})

	recorder := httptest.NewRecorder()
	rp.serveRoute(recorder, httptest.NewRequest(http.MethodGet, "/api/late/object", nil), compiledOld)
	if got := recorder.Body.String(); got != "old-in-flight" {
		oldSnapshot.release()
		t.Fatalf("in-flight old route returned %q", got)
	}
	select {
	case <-oldClosed:
		oldSnapshot.release()
		t.Fatal("retired snapshot closed its pool before the in-flight request released it")
	default:
	}

	oldSnapshot.release()
	select {
	case <-oldClosed:
	case <-time.After(2 * time.Second):
		t.Fatal("late connection created by retired snapshot remained idle after release")
	}
}

func TestConcurrentRouteTableReplacementHasNoStaleFinalBinding(t *testing.T) {
	backend := func(label string) *httptest.Server {
		return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			_, _ = io.WriteString(w, label)
		}))
	}
	backendA := backend("a")
	defer backendA.Close()
	backendB := backend("b")
	defer backendB.Close()

	routeFor := func(target string) servicestatus.ServiceRoute {
		route := ancestorStorageRoute("storage.object.get", http.MethodGet, target, true)
		route.Prefix = "/api/concurrent"
		return route
	}
	rp := newTestServiceProxy(t, nil)
	rp.SetRouteTable(servicestatus.RouteTable{Version: "a", Routes: []servicestatus.ServiceRoute{routeFor(backendA.URL)}, CanProxy: true})
	gateway := httptest.NewServer(rp)
	defer gateway.Close()

	errorsSeen := make(chan error, 32)
	var requests sync.WaitGroup
	for worker := 0; worker < 16; worker++ {
		requests.Add(1)
		go func() {
			defer requests.Done()
			for i := 0; i < 40; i++ {
				resp, err := http.Get(gateway.URL + "/api/concurrent/object")
				if err != nil {
					errorsSeen <- err
					return
				}
				body, readErr := io.ReadAll(resp.Body)
				_ = resp.Body.Close()
				if readErr != nil {
					errorsSeen <- readErr
					return
				}
				if value := string(body); value != "a" && value != "b" {
					errorsSeen <- fmt.Errorf("unexpected binding response %q", value)
					return
				}
			}
		}()
	}
	for revision := 0; revision < 80; revision++ {
		target := backendA.URL
		version := "a"
		if revision%2 == 1 {
			target = backendB.URL
			version = "b"
		}
		rp.SetRouteTable(servicestatus.RouteTable{Version: version, Routes: []servicestatus.ServiceRoute{routeFor(target)}, CanProxy: true})
	}
	requests.Wait()
	close(errorsSeen)
	for err := range errorsSeen {
		t.Error(err)
	}

	rp.SetRouteTable(servicestatus.RouteTable{Version: "final-b", Routes: []servicestatus.ServiceRoute{routeFor(backendB.URL)}, CanProxy: true})
	for i := 0; i < 32; i++ {
		if got := proxyResponseBody(t, gateway.URL+"/api/concurrent/object"); got != "b" {
			t.Fatalf("final revision request %d used stale binding %q", i, got)
		}
	}
}

func proxyResponseBody(t *testing.T, endpoint string) string {
	t.Helper()
	resp, err := http.Get(endpoint)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatal(err)
	}
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("proxy response status=%d body=%s", resp.StatusCode, body)
	}
	return string(body)
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
	t.Cleanup(rp.Close)
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
