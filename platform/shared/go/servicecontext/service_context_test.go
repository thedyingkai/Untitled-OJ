package servicecontext

import (
	"context"
	"crypto/sha256"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
)

func testContext(t *testing.T, origin string) ServiceContext {
	t.Helper()
	root := t.TempDir()
	token := filepath.Join(root, "token")
	if err := os.WriteFile(token, []byte("first-token"), 0o600); err != nil {
		t.Fatal(err)
	}
	return ServiceContext{
		SchemaVersion: 1,
		Deployment:    DeploymentIdentity{ID: "deployment-b", Service: "fixture-consumer", Node: "node-b"},
		Gateway:       GatewayContext{Origin: origin},
		Bindings: map[string]APIBinding{
			"storage_get": {BindingID: "binding-1", APIID: "storage.object.get", BasePath: "/internal/apis/storage.object.get", TimeoutMS: 300_000},
		},
		CredentialFile: token,
		Generation:     3,
	}
}

func TestBindingAndCredentialRotationAreGeneric(t *testing.T) {
	value := testContext(t, "http://127.0.0.1:8080")
	if err := value.Validate(); err != nil {
		t.Fatal(err)
	}
	if got, err := value.BindingURL("storage_get", "/objects/42?download=1"); err != nil || got != "http://127.0.0.1:8080/internal/apis/storage.object.get/objects/42?download=1" {
		t.Fatalf("unexpected binding URL %q: %v", got, err)
	}
	if err := value.RequireService("fixture-consumer"); err != nil {
		t.Fatal(err)
	}
	if err := value.RequireService("judge-worker"); err == nil {
		t.Fatal("expected service identity mismatch")
	}
	request, err := value.NewRequest(context.Background(), "storage_get", http.MethodPost, "/objects", nil)
	if err != nil {
		t.Fatal(err)
	}
	if got := request.Header.Get("Authorization"); got != "Bearer first-token" {
		t.Fatalf("unexpected first credential %q", got)
	}
	if request.Header.Get("Idempotency-Key") == "" {
		t.Fatal("mutation is missing Idempotency-Key")
	}
	if err := os.WriteFile(value.CredentialFile, []byte("second-token"), 0o600); err != nil {
		t.Fatal(err)
	}
	request, err = value.NewRequest(context.Background(), "storage_get", http.MethodGet, "/objects/42", nil)
	if err != nil {
		t.Fatal(err)
	}
	if got := request.Header.Get("Authorization"); got != "Bearer second-token" {
		t.Fatalf("credential was not reloaded: %q", got)
	}
}

func TestRequestOptionsCannotReplaceWorkloadIdentity(t *testing.T) {
	value := testContext(t, "http://127.0.0.1:8080")
	request, err := value.NewRequestWithOptions(
		context.Background(),
		"storage_get",
		http.MethodPut,
		"/objects/42",
		nil,
		RequestOptions{
			Headers: http.Header{
				"Authorization":   []string{"Bearer attacker"},
				"If-None-Match":   []string{"*"},
				"Idempotency-Key": []string{"stable-operation"},
			},
			ContentLength: 42,
		},
	)
	if err != nil {
		t.Fatal(err)
	}
	if got := request.Header.Get("Authorization"); got != "Bearer first-token" {
		t.Fatalf("application replaced workload identity: %q", got)
	}
	if got := request.Header.Get("Idempotency-Key"); got != "stable-operation" {
		t.Fatalf("explicit stable idempotency key was not preserved: %q", got)
	}
	if request.ContentLength != 42 || request.Header.Get("If-None-Match") != "*" {
		t.Fatal("request options were not applied")
	}
}

func TestDownloadToStreamsAndVerifiesIdentity(t *testing.T) {
	payload := []byte("service-contract-v2-artifact")
	digest := sha256.Sum256(payload)
	server := httptest.NewServer(http.HandlerFunc(func(response http.ResponseWriter, request *http.Request) {
		if request.URL.Path != "/internal/apis/storage.object.get/objects/sha256/test" {
			http.NotFound(response, request)
			return
		}
		if request.Header.Get("Authorization") != "Bearer first-token" {
			http.Error(response, "unauthorized", http.StatusUnauthorized)
			return
		}
		response.Header().Set("Content-Length", fmt.Sprint(len(payload)))
		_, _ = response.Write(payload)
	}))
	defer server.Close()
	value := testContext(t, server.URL)
	client, err := value.Client()
	if err != nil {
		t.Fatal(err)
	}
	target := filepath.Join(t.TempDir(), "artifact.bin")
	if err := value.DownloadTo(context.Background(), client, "storage_get", "/objects/sha256/test", "sha256:"+fmt.Sprintf("%x", digest), uint64(len(payload)), target); err != nil {
		t.Fatal(err)
	}
	actual, err := os.ReadFile(target)
	if err != nil {
		t.Fatal(err)
	}
	if string(actual) != string(payload) {
		t.Fatalf("unexpected artifact %q", actual)
	}
}

func TestRejectsAbsoluteAndEscapingBindingPaths(t *testing.T) {
	value := testContext(t, "http://127.0.0.1:8080")
	for _, invalid := range []string{"https://attacker.example/object", "//attacker.example/object", "/../admin"} {
		if _, err := value.BindingURL("storage_get", invalid); err == nil {
			t.Fatalf("expected %q to be rejected", invalid)
		}
	}
}

func TestEmptyOptionalBindingSetIsValidButLookupRemainsExplicit(t *testing.T) {
	value := testContext(t, "https://gateway.example")
	value.Bindings = map[string]APIBinding{}
	if err := value.Validate(); err != nil {
		t.Fatalf("empty optional binding set must remain mountable: %v", err)
	}
	if _, err := value.Binding("storage_get"); err == nil {
		t.Fatal("missing named binding must not be inferred")
	}
}

func TestGatewayOriginRejectsUserInfoAndNonOriginComponents(t *testing.T) {
	for _, origin := range []string{
		"https://user@gateway.example",
		"https://gateway.example/path",
		"https://gateway.example?query=1",
		"https://gateway.example#fragment",
	} {
		value := testContext(t, origin)
		if err := value.Validate(); err == nil {
			t.Fatalf("expected invalid gateway origin %q", origin)
		}
	}
}
