package httpapi

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"ojos-contest-service/internal/contest"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
)

func testHandler(t *testing.T) http.Handler {
	t.Helper()
	handler, err := New(contest.NewMemoryRepository(), nil, nil, slog.New(slog.NewJSONHandler(io.Discard, nil)), prometheus.NewRegistry())
	if err != nil {
		t.Fatal(err)
	}
	return handler.Routes()
}

func TestCRUDAndMetrics(t *testing.T) {
	handler := testHandler(t)
	start := time.Date(2027, 2, 1, 9, 0, 0, 0, time.UTC)
	created := requestJSON(t, handler, http.MethodPost, "/contests", map[string]any{
		"slug": "spring-cup", "title": "Spring Cup", "description": "Open contest",
		"startsAt": start, "endsAt": start.Add(3 * time.Hour),
	}, http.StatusCreated)
	id := int64(created["id"].(float64))
	requestJSON(t, handler, http.MethodGet, "/contests", nil, http.StatusOK)
	requestJSON(t, handler, http.MethodGet, "/contests/"+jsonNumber(id), nil, http.StatusOK)
	requestJSON(t, handler, http.MethodPut, "/contests/"+jsonNumber(id), map[string]any{
		"title": "Spring Cup Finals", "description": "", "startsAt": start,
		"endsAt": start.Add(4 * time.Hour), "version": 1,
	}, http.StatusOK)
	requestJSON(t, handler, http.MethodDelete, "/contests/"+jsonNumber(id), nil, http.StatusNoContent)
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/metrics", nil))
	if recorder.Code != http.StatusOK || !strings.Contains(recorder.Body.String(), "ojos_contest_service_http_requests_total") {
		t.Fatalf("metrics status=%d body=%s", recorder.Code, recorder.Body.String())
	}
}

func TestInvalidAndConflictErrorsAreStable(t *testing.T) {
	handler := testHandler(t)
	start := time.Date(2027, 2, 1, 9, 0, 0, 0, time.UTC)
	body := map[string]any{"slug": "spring-cup", "title": "Spring Cup", "startsAt": start, "endsAt": start.Add(time.Hour)}
	requestJSON(t, handler, http.MethodPost, "/contests", body, http.StatusCreated)
	conflict := requestJSON(t, handler, http.MethodPost, "/contests", body, http.StatusConflict)
	if conflict["code"] != "contest_conflict" {
		t.Fatalf("conflict = %#v", conflict)
	}
	bad := httptest.NewRecorder()
	handler.ServeHTTP(bad, httptest.NewRequest(http.MethodPost, "/contests", strings.NewReader(`{"slug":"ok","unknown":true}`)))
	if bad.Code != http.StatusBadRequest || strings.Contains(bad.Body.String(), "unknown") {
		t.Fatalf("bad response status=%d body=%s", bad.Code, bad.Body.String())
	}
}

func TestHealthIsProcessLiveness(t *testing.T) {
	recorder := httptest.NewRecorder()
	testHandler(t).ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/healthz", nil))
	if recorder.Code != http.StatusOK || !strings.Contains(recorder.Body.String(), `"status":"ok"`) {
		t.Fatalf("health status=%d body=%s", recorder.Code, recorder.Body.String())
	}
}

type failingProblemReader struct{}

func (failingProblemReader) Probe(context.Context) error { return errors.New("problem unavailable") }

func TestReadinessIncludesRequiredAPIProbe(t *testing.T) {
	handler, err := New(contest.NewMemoryRepository(), failingProblemReader{}, nil, slog.New(slog.NewJSONHandler(io.Discard, nil)), prometheus.NewRegistry())
	if err != nil {
		t.Fatal(err)
	}
	recorder := httptest.NewRecorder()
	handler.Routes().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/readyz", nil))
	if recorder.Code != http.StatusServiceUnavailable || !strings.Contains(recorder.Body.String(), `"code":"problem_api_unavailable"`) {
		t.Fatalf("readiness status=%d body=%s", recorder.Code, recorder.Body.String())
	}
}

type recordingPermissionChecker struct {
	permission string
	err        error
}

func (checker *recordingPermissionChecker) RequireUserPermission(_ context.Context, _ int64, permission string, _ sharedperm.Scope) error {
	checker.permission = permission
	return checker.err
}

func (checker *recordingPermissionChecker) HasUserPermission(_ context.Context, _ int64, _ string, _ sharedperm.Scope) (bool, error) {
	return checker.err == nil, checker.err
}

func TestOperationPermissionIsEnforcedByProvider(t *testing.T) {
	checker := &recordingPermissionChecker{}
	handler, err := New(contest.NewMemoryRepository(), nil, checker, slog.New(slog.NewJSONHandler(io.Discard, nil)), prometheus.NewRegistry())
	if err != nil {
		t.Fatal(err)
	}
	request := httptest.NewRequest(http.MethodGet, "/contests", nil)
	request.Header.Set(authctx.HeaderAuthVerified, "true")
	request.Header.Set(authctx.HeaderUserID, "42")
	response := httptest.NewRecorder()
	handler.Routes().ServeHTTP(response, request)
	if response.Code != http.StatusOK || checker.permission != "contest-service.contest.read" {
		t.Fatalf("status=%d permission=%q body=%s", response.Code, checker.permission, response.Body.String())
	}

	checker.err = sharedperm.ErrForbidden
	response = httptest.NewRecorder()
	handler.Routes().ServeHTTP(response, request)
	if response.Code != http.StatusForbidden {
		t.Fatalf("provider did not fail closed on denied permission: status=%d body=%s", response.Code, response.Body.String())
	}
}

func TestPermissionTableMatchesPublishedOperations(t *testing.T) {
	for operation, expected := range map[string]string{
		"listContests":      "contest-service.contest.read",
		"getContest":        "contest-service.contest.read",
		"createContest":     "contest-service.contest.manage",
		"updateContest":     "contest-service.contest.manage",
		"deleteContest":     "contest-service.contest.manage",
		"adminListContests": "contest-service.contest.manage",
	} {
		if actual := RequiredPermission(operation); actual != expected {
			t.Fatalf("%s permission=%q want=%q", operation, actual, expected)
		}
	}
	if permission := RequiredPermission("contestHealth"); permission != "" {
		t.Fatalf("health permission=%q", permission)
	}
	if permission := RequiredPermission("contestReady"); permission != "" {
		t.Fatalf("readiness permission=%q", permission)
	}
}

func TestStatusRecorderPreservesFirstStatus(t *testing.T) {
	recorder := httptest.NewRecorder()
	status := &statusRecorder{ResponseWriter: recorder}
	status.WriteHeader(http.StatusCreated)
	status.WriteHeader(http.StatusInternalServerError)
	if status.status != http.StatusCreated || recorder.Code != http.StatusCreated {
		t.Fatalf("status=%d recorder=%d", status.status, recorder.Code)
	}
}

func requestJSON(t *testing.T, handler http.Handler, method, path string, input any, status int) map[string]any {
	t.Helper()
	var body []byte
	if input != nil {
		body, _ = json.Marshal(input)
	}
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(method, path, bytes.NewReader(body)))
	if recorder.Code != status {
		t.Fatalf("%s %s status=%d want=%d body=%s", method, path, recorder.Code, status, recorder.Body.String())
	}
	if status == http.StatusNoContent {
		return nil
	}
	var value map[string]any
	if err := json.Unmarshal(recorder.Body.Bytes(), &value); err != nil {
		t.Fatal(err)
	}
	return value
}

func jsonNumber(value int64) string {
	return strconv.FormatInt(value, 10)
}
