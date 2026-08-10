package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"

	"ojos-auth-service/internal/repository"
	"ojos-auth-service/internal/service"
	"ojos-auth-service/internal/svc"
	"ojos-auth-service/internal/types"
)

type handlerBootstrapStore struct {
	calls int
	err   error
}

func (s *handlerBootstrapStore) BootstrapAdmin(
	_ context.Context,
	_ string,
	_ string,
	_ string,
) (int64, error) {
	s.calls++
	return 73, s.err
}

func TestAdminBootstrapRouteIsConditionalAndUnauthenticatedByJWT(t *testing.T) {
	data, err := os.ReadFile("routes.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, required := range []string{
		"if serverCtx.AdminBootstrap != nil",
		`Path:    "/bootstrap/admin"`,
		"Handler: adminBootstrapHandler(serverCtx)",
	} {
		if !strings.Contains(source, required) {
			t.Fatalf("bootstrap route lost condition %q", required)
		}
	}
	bootstrapIndex := strings.Index(source, `Path:    "/bootstrap/admin"`)
	authMiddlewareIndex := strings.Index(source, "serverCtx.AuthMiddleware")
	if bootstrapIndex < 0 || authMiddlewareIndex < 0 || bootstrapIndex > authMiddlewareIndex {
		t.Fatal("bootstrap route must be separately guarded by its one-time credential, not JWT middleware")
	}
}

func TestAdminBootstrapHandlerParsesHeaderAndReturnsCreated(t *testing.T) {
	secret := strings.Repeat("bootstrap-", 4)
	store := &handlerBootstrapStore{}
	bootstrap, err := service.NewAdminBootstrapService(store, []byte(secret))
	if err != nil {
		t.Fatal(err)
	}
	serverContext := &svc.ServiceContext{AdminBootstrap: bootstrap}
	request := httptest.NewRequest(
		http.MethodPost,
		"/auth/bootstrap/admin",
		bytes.NewBufferString(`{"username":"initial-admin","password":"correct horse battery staple"}`),
	)
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("X-OJOS-Bootstrap-Secret", secret)
	response := httptest.NewRecorder()

	adminBootstrapHandler(serverContext).ServeHTTP(response, request)

	if response.Code != http.StatusCreated {
		t.Fatalf("expected 201, got %d: %s", response.Code, response.Body.String())
	}
	var body types.AdminBootstrapResp
	if err := json.Unmarshal(response.Body.Bytes(), &body); err != nil {
		t.Fatal(err)
	}
	if body.Code != 0 || body.Data.UserId != 73 || body.Data.Username != "initial-admin" {
		t.Fatalf("unexpected response: %#v", body)
	}
	if store.calls != 1 {
		t.Fatalf("expected one database call, got %d", store.calls)
	}
	if response.Header().Get("Cache-Control") != "no-store" {
		t.Fatal("bootstrap response must not be cached")
	}
}

func TestAdminBootstrapHandlerHidesConsumedStateFromInvalidSecret(t *testing.T) {
	secret := strings.Repeat("bootstrap-", 4)
	store := &handlerBootstrapStore{err: repository.ErrAdminBootstrapConsumed}
	bootstrap, err := service.NewAdminBootstrapService(store, []byte(secret))
	if err != nil {
		t.Fatal(err)
	}
	serverContext := &svc.ServiceContext{AdminBootstrap: bootstrap}
	body := `{"username":"initial-admin","password":"correct horse battery staple"}`

	invalidRequest := httptest.NewRequest(http.MethodPost, "/auth/bootstrap/admin", bytes.NewBufferString(body))
	invalidRequest.Header.Set("Content-Type", "application/json")
	invalidRequest.Header.Set("X-OJOS-Bootstrap-Secret", strings.Repeat("wrong---", 4))
	invalidResponse := httptest.NewRecorder()
	adminBootstrapHandler(serverContext).ServeHTTP(invalidResponse, invalidRequest)
	if invalidResponse.Code != http.StatusForbidden || store.calls != 0 {
		t.Fatalf("invalid secret leaked state: status=%d calls=%d", invalidResponse.Code, store.calls)
	}

	validRequest := httptest.NewRequest(http.MethodPost, "/auth/bootstrap/admin", bytes.NewBufferString(body))
	validRequest.Header.Set("Content-Type", "application/json")
	validRequest.Header.Set("X-OJOS-Bootstrap-Secret", secret)
	validResponse := httptest.NewRecorder()
	adminBootstrapHandler(serverContext).ServeHTTP(validResponse, validRequest)
	if validResponse.Code != http.StatusConflict || store.calls != 1 {
		t.Fatalf("consumed bootstrap mismatch: status=%d calls=%d", validResponse.Code, store.calls)
	}
}
