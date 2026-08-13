package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"ojos-problem-service/internal/artifactgc"
	problemmw "ojos-problem-service/internal/middleware"
	"ojos-problem-service/internal/svc"
	sharedmw "ojos-shared/middleware"
	sharedperm "ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/service"
	"github.com/zeromicro/go-zero/rest"
	patrouter "github.com/zeromicro/go-zero/rest/router"
)

type artifactGCRoutePermission struct{}

func (artifactGCRoutePermission) RequireUserPermission(ctx context.Context, userID int64, permission string, scope sharedperm.Scope) error {
	allowed, err := artifactGCRoutePermission{}.HasUserPermission(ctx, userID, permission, scope)
	if err != nil {
		return err
	}
	if !allowed {
		return sharedperm.ErrForbidden
	}
	return nil
}

func (artifactGCRoutePermission) HasUserPermission(_ context.Context, userID int64, permission string, scope sharedperm.Scope) (bool, error) {
	return userID == 42 && permission == "problem.manage.data" && scope.Type == sharedperm.SystemScope().Type, nil
}

type artifactGCRouteLedger struct {
	reconcileCalls int
	retryCalls     int
}

func (*artifactGCRouteLedger) ListIntents(context.Context, string, string, int) (artifactgc.IntentPage, error) {
	return artifactgc.IntentPage{}, nil
}

func (*artifactGCRouteLedger) RecoveryDue(context.Context) (bool, error) { return false, nil }

func (l *artifactGCRouteLedger) RequestReconcile(context.Context, string, string, int64, string, string, string) (artifactgc.OperatorActionResult, error) {
	l.reconcileCalls++
	return artifactgc.OperatorActionResult{ActionID: 11, FromStatus: "PENDING", ToStatus: "PENDING"}, nil
}

func (l *artifactGCRouteLedger) RetryNeedsAttention(context.Context, string, int, string, string, string) (artifactgc.OperatorActionResult, error) {
	l.retryCalls++
	return artifactgc.OperatorActionResult{ActionID: 12, FromStatus: "NEEDS_ATTENTION", ToStatus: "PENDING"}, nil
}

func TestArtifactGCActionRoutesUseLiteralColonAndReturnAccepted(t *testing.T) {
	sharedmw.InstallHTTPErrorHandler()
	ledger := &artifactGCRouteLedger{}
	router := registeredProblemRouter(t, &svc.ServiceContext{
		Permission:            artifactGCRoutePermission{},
		ArtifactGC:            svc.NewArtifactGCController(ledger, artifactgc.Collector{}),
		UserContextMiddleware: problemmw.NewUserContextMiddleware().Handle,
	})

	tests := []struct {
		path string
		body map[string]any
	}{
		{
			path: "/problem/admin/artifact-gc/intents:reconcile",
			body: map[string]any{
				"artifact_uri":        "storage://problems/package-sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.zip",
				"artifact_sha256":     "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
				"artifact_size_bytes": 17,
				"reason":              "operator verified",
			},
		},
		{
			path: "/problem/admin/artifact-gc/intents:retry",
			body: map[string]any{
				"artifact_uri":           "storage://problems/package-sha256-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.zip",
				"expected_failure_count": 3,
				"reason":                 "operator verified",
			},
		},
	}
	for _, tt := range tests {
		payload, err := json.Marshal(tt.body)
		if err != nil {
			t.Fatal(err)
		}
		req := httptest.NewRequest(http.MethodPost, tt.path, bytes.NewReader(payload))
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("X-Auth-Verified", "true")
		req.Header.Set("X-User-Id", "42")
		req.Header.Set("Idempotency-Key", "route-test-"+tt.path)
		rec := httptest.NewRecorder()
		router.ServeHTTP(rec, req)
		if rec.Code != http.StatusAccepted {
			t.Fatalf("POST %s status=%d body=%s", tt.path, rec.Code, rec.Body.String())
		}
	}
	if ledger.reconcileCalls != 1 || ledger.retryCalls != 1 {
		t.Fatalf("literal action routes reached wrong handlers: reconcile=%d retry=%d", ledger.reconcileCalls, ledger.retryCalls)
	}

	for _, path := range []string{
		"/admin/artifact-gc/intents/reconcile",
		"/admin/artifact-gc/intents/retry",
	} {
		body := map[string]any{"artifact_uri": "storage://problems/package-sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.zip", "reason": "slash alias"}
		if strings.HasSuffix(path, "/reconcile") {
			body["artifact_sha256"] = strings.Repeat("a", 64)
			body["artifact_size_bytes"] = 17
		} else {
			body["expected_failure_count"] = 2
		}
		payload, err := json.Marshal(body)
		if err != nil {
			t.Fatal(err)
		}
		req := httptest.NewRequest(http.MethodPost, path, bytes.NewReader(payload))
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("X-Auth-Verified", "true")
		req.Header.Set("X-User-Id", "42")
		req.Header.Set("Idempotency-Key", "slash-route-test-"+path)
		rec := httptest.NewRecorder()
		router.ServeHTTP(rec, req)
		if rec.Code != http.StatusAccepted {
			t.Fatalf("signed slash route %s failed: status=%d body=%s", path, rec.Code, rec.Body.String())
		}
	}
	if ledger.reconcileCalls != 2 || ledger.retryCalls != 2 {
		t.Fatalf("colon/slash compatibility routes reached wrong handlers: reconcile=%d retry=%d", ledger.reconcileCalls, ledger.retryCalls)
	}
}

func TestArtifactGCUnavailableIsServiceUnavailable(t *testing.T) {
	sharedmw.InstallHTTPErrorHandler()
	router := registeredProblemRouter(t, &svc.ServiceContext{
		Permission:            artifactGCRoutePermission{},
		UserContextMiddleware: problemmw.NewUserContextMiddleware().Handle,
	})
	payload := []byte(`{"artifact_uri":"storage://problems/package-sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.zip","artifact_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","artifact_size_bytes":17,"reason":"operator verified"}`)
	req := httptest.NewRequest(http.MethodPost, "/problem/admin/artifact-gc/intents:reconcile", bytes.NewReader(payload))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-Auth-Verified", "true")
	req.Header.Set("X-User-Id", "42")
	req.Header.Set("Idempotency-Key", "unavailable-test")
	rec := httptest.NewRecorder()
	router.ServeHTTP(rec, req)
	if rec.Code != http.StatusServiceUnavailable {
		t.Fatalf("disabled artifact GC status=%d body=%s", rec.Code, rec.Body.String())
	}
}

func registeredProblemRouter(t *testing.T, serverCtx *svc.ServiceContext) http.Handler {
	t.Helper()
	server, err := rest.NewServer(rest.RestConf{
		ServiceConf: service.ServiceConf{Name: "artifact-gc-route-test", Mode: "test"},
		Host:        "127.0.0.1",
		Port:        18883,
	})
	if err != nil {
		t.Fatal(err)
	}
	RegisterHandlers(server, serverCtx)
	router := patrouter.NewRouter()
	for _, route := range server.Routes() {
		if err := router.Handle(route.Method, route.Path, route.Handler); err != nil {
			t.Fatalf("register %s %s: %v", route.Method, route.Path, err)
		}
	}
	return router
}
