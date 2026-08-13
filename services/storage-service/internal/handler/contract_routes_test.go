package handler

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"ojos-shared/security/workload"
	storagemw "ojos-storage-service/internal/middleware"
	"ojos-storage-service/internal/svc"

	"github.com/zeromicro/go-zero/rest"
)

type contractRoute struct {
	APIID        string `json:"apiId"`
	Audience     string `json:"audience"`
	Method       string `json:"method"`
	ProviderPath string `json:"providerPath"`
}

func TestProductionRegistersOnlySignedOperationsAndAnonymousHealth(t *testing.T) {
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	issuer, err := workload.NewIssuer(privateKey, "workload-1", "issuer", "gateway", time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	verifier, err := workload.NewVerifier(publicKey, "workload-1", "issuer", "gateway")
	if err != nil {
		t.Fatal(err)
	}
	token, _, err := issuer.Issue(workload.IssueRequest{
		DeploymentID: "deployment-problem", ServiceID: "problem-service", NodeID: "node-a", CredentialGeneration: 1,
	}, time.Now())
	if err != nil {
		t.Fatal(err)
	}

	routes := productionRoutes(&svc.ServiceContext{WorkloadAuthEnabled: true, WorkloadVerifier: verifier})
	if len(routes) != 7 {
		t.Fatalf("production route count = %d, want 7", len(routes))
	}
	for _, route := range routes {
		if strings.HasPrefix(route.Path, "/api/storage") || strings.Contains(route.Path, "metadata") || strings.Contains(route.Path, "buckets") {
			t.Fatalf("production registered unsigned legacy route %s %s", route.Method, route.Path)
		}
	}

	put := findRoute(t, routes, http.MethodPut, "/:bucket/:key")
	request, _ := http.NewRequest(http.MethodPut, "http://storage/problems/object", nil)
	request.Header.Set("Authorization", "Bearer "+token)
	request.Header.Set("X-OJOS-Gateway-Proxy", "service-routing")
	request.Header.Set("X-OJOS-Caller-Service", "problem-service")
	request.Header.Set("X-OJOS-Caller-Node-Id", "node-a")
	request.Header.Set("X-OJOS-Caller-Deployment-Id", "deployment-problem")
	request.Header.Set("X-OJOS-Binding-Id", "binding-put")
	request.Header.Set("X-OJOS-Api-Id", "storage.object.get")
	allowed := false
	put.Handler = storagemw.NewWorkloadAuthMiddleware(true, verifier, "storage.object.put").Handle(func(http.ResponseWriter, *http.Request) { allowed = true })
	put.Handler(newResponseRecorder(), request)
	if allowed {
		t.Fatal("read Binding reached put operation")
	}
}

func productionRoutes(serverCtx *svc.ServiceContext) []rest.Route {
	return append([]rest.Route{
		{Method: http.MethodGet, Path: "/health", Handler: healthHandler(serverCtx)},
		{Method: http.MethodGet, Path: "/healthz", Handler: healthHandler(serverCtx)},
		{Method: http.MethodGet, Path: "/readyz", Handler: readyHandler(serverCtx)},
	}, authoritativeObjectRoutes(serverCtx)...)
}

func findRoute(t *testing.T, routes []rest.Route, method, path string) rest.Route {
	t.Helper()
	for _, route := range routes {
		if route.Method == method && route.Path == path {
			return route
		}
	}
	t.Fatalf("missing route %s %s", method, path)
	return rest.Route{}
}

type responseRecorder struct {
	header http.Header
	status int
}

func newResponseRecorder() *responseRecorder               { return &responseRecorder{header: http.Header{}} }
func (r *responseRecorder) Header() http.Header            { return r.header }
func (r *responseRecorder) Write(body []byte) (int, error) { return len(body), nil }
func (r *responseRecorder) WriteHeader(status int)         { r.status = status }

func TestGeneratedContractMatchesAuthoritativeRuntimeRoutes(t *testing.T) {
	bytes, err := os.ReadFile(filepath.Join("..", "..", "gen", "service.contract.json"))
	if err != nil {
		t.Fatal(err)
	}
	var contract struct {
		Operations []contractRoute `json:"operations"`
	}
	if err := json.Unmarshal(bytes, &contract); err != nil {
		t.Fatal(err)
	}

	want := []contractRoute{
		{APIID: "storage.object.put", Audience: "internal", Method: http.MethodPut, ProviderPath: "/{bucket}/{key}"},
		{APIID: "storage.object.get", Audience: "internal", Method: http.MethodGet, ProviderPath: "/{bucket}/{key}"},
		{APIID: "storage.object.head", Audience: "internal", Method: http.MethodHead, ProviderPath: "/{bucket}/{key}"},
		{APIID: "storage.object.delete", Audience: "internal", Method: http.MethodDelete, ProviderPath: "/{bucket}/{key}"},
		{APIID: "storage.object.get", Audience: "internal", Method: http.MethodGet, ProviderPath: "/healthz"},
		{APIID: "storage.object.get", Audience: "internal", Method: http.MethodGet, ProviderPath: "/readyz"},
	}
	got := append([]contractRoute(nil), contract.Operations...)
	sortRoutes(got)
	sortRoutes(want)
	if len(got) != len(want) {
		t.Fatalf("operation count = %d, want %d", len(got), len(want))
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("operation[%d] = %#v, want %#v", i, got[i], want[i])
		}
	}

	// A v3 ApiBinding targets the surface base path "/" and the client appends
	// the provider path. Therefore the service must register the unprefixed
	// route; making /api/storage authoritative would produce a deterministic
	// Gateway 404 even though the legacy direct endpoint still works.
	for _, route := range authoritativeObjectRoutes(&svc.ServiceContext{}) {
		if strings.HasPrefix(route.Path, "/api/storage") {
			t.Fatalf("authoritative provider route leaked legacy prefix: %s", route.Path)
		}
	}
}

func TestGeneratedClientsComposeBindingBaseWithRelativeProviderPath(t *testing.T) {
	bytes, err := os.ReadFile(filepath.Join("..", "..", "gen", "go", "client.go"))
	if err != nil {
		t.Fatal(err)
	}
	source := string(bytes)
	if !strings.Contains(source, `Path: "/{bucket}/{key}"`) {
		t.Fatal("generated Go client does not preserve the relative provider path")
	}
	base := "https://gateway.internal/internal/apis/storage.object.put"
	path := strings.ReplaceAll(strings.ReplaceAll("/{bucket}/{key}", "{bucket}", "problems"), "{key}", "package.zip")
	if got, want := strings.TrimRight(base, "/")+path, "https://gateway.internal/internal/apis/storage.object.put/problems/package.zip"; got != want {
		t.Fatalf("Binding base + generated provider path = %s, want %s", got, want)
	}
}

func sortRoutes(routes []contractRoute) {
	sort.Slice(routes, func(i, j int) bool {
		left := routes[i].APIID + routes[i].Method + routes[i].ProviderPath
		right := routes[j].APIID + routes[j].Method + routes[j].ProviderPath
		return left < right
	})
}
