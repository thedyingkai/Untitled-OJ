package problemclient

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"

	"ojos-shared/servicecontext"
)

func TestProbeWithoutProviderReturnsTypedBindingUnavailable(t *testing.T) {
	err := (*Client)(nil).Probe(context.Background())
	var unavailable *servicecontext.BindingUnavailable
	if !errors.As(err, &unavailable) || unavailable.Name != BindingName {
		t.Fatalf("error = %v, want BindingUnavailable for %q", err, BindingName)
	}
}

func TestProbeAddressesBindingRootWithoutRepeatingProviderPath(t *testing.T) {
	var requestURI string
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		requestURI = request.RequestURI
		if request.RequestURI != "/internal/apis/problem.problem.read" {
			http.NotFound(writer, request)
			return
		}
		writer.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()

	root := t.TempDir()
	credentialFile := filepath.Join(root, "credential")
	if err := os.WriteFile(credentialFile, []byte("workload-token"), 0o600); err != nil {
		t.Fatal(err)
	}
	contextFile := filepath.Join(root, "context.json")
	snapshot := servicecontext.ServiceContext{
		SchemaVersion: 1,
		Deployment: servicecontext.DeploymentIdentity{
			ID: "contest-deployment", Service: "contest-service", Node: "node-a",
		},
		Gateway: servicecontext.GatewayContext{Origin: server.URL},
		Bindings: map[string]servicecontext.APIBinding{
			BindingName: {
				BindingID: "problem-binding", APIID: BindingName,
				BasePath: "/internal/apis/problem.problem.read", TimeoutMS: 1_000,
			},
		},
		CredentialFile: credentialFile,
		Generation:     1,
	}
	encoded, err := json.Marshal(snapshot)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(contextFile, encoded, 0o600); err != nil {
		t.Fatal(err)
	}
	provider, err := servicecontext.NewContextProvider(contextFile, servicecontext.ProviderOptions{})
	if err != nil {
		t.Fatal(err)
	}
	defer provider.Close()

	if err := New(provider).Probe(context.Background()); err != nil {
		t.Fatalf("Probe() error = %v", err)
	}
	if requestURI != "/internal/apis/problem.problem.read" {
		t.Fatalf("request URI = %q, want binding root", requestURI)
	}
}
