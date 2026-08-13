package svc

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"encoding/pem"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ojos-storage-service/internal/config"
)

func TestManagedContextRequiresAgentPathAndParsesInMemoryEd25519Key(t *testing.T) {
	publicKey, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	der, err := x509.MarshalPKIXPublicKey(publicKey)
	if err != nil {
		t.Fatal(err)
	}
	key := string(pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: der}))
	keyPath := filepath.Join(t.TempDir(), "workload-public-key.pem")
	if err := os.WriteFile(keyPath, []byte(key), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_MANAGED_WORKLOAD", "true")
	value := testConfig(t, "")
	value.WorkloadIdentity.PublicKeyFile = keyPath

	_, err = BuildServiceContext(value)
	if err == nil || !strings.Contains(err.Error(), "service context path") {
		t.Fatalf("missing Agent context path was not rejected: %v", err)
	}
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "/run/ojos/service/context.json")
	ctx, err := BuildServiceContext(value)
	if err != nil {
		t.Fatal(err)
	}
	defer ctx.Close(t.Context())
	if !ctx.WorkloadAuthEnabled || ctx.WorkloadVerifier == nil {
		t.Fatal("managed storage did not enable the parsed workload verifier")
	}
}

func TestWorkloadVerifierRejectsNonEd25519MultipleAndOversizedFiles(t *testing.T) {
	rsaPrivate, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	rsaDER, err := x509.MarshalPKIXPublicKey(&rsaPrivate.PublicKey)
	if err != nil {
		t.Fatal(err)
	}
	edPublic, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	edDER, err := x509.MarshalPKIXPublicKey(edPublic)
	if err != nil {
		t.Fatal(err)
	}
	edPEM := pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: edDER})
	for name, contents := range map[string][]byte{
		"rsa":       pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: rsaDER}),
		"multiple":  append(append([]byte{}, edPEM...), edPEM...),
		"oversized": bytes.Repeat([]byte("A"), int(maximumWorkloadPublicKeyBytes)+1),
	} {
		t.Run(name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "public.pem")
			if err := os.WriteFile(path, contents, 0o600); err != nil {
				t.Fatal(err)
			}
			_, err := workloadVerifier(config.WorkloadIdentityConfig{
				PublicKeyFile: path, KeyID: "workload-1", Issuer: "issuer", Audience: "audience",
			})
			if err == nil {
				t.Fatal("invalid workload verifier file was accepted")
			}
		})
	}
}

func TestProductionContextFailsClosedWithoutVerifier(t *testing.T) {
	t.Setenv("OJOS_ENVIRONMENT", "production")
	_, err := BuildServiceContext(testConfig(t, ""))
	if err == nil || !strings.Contains(err.Error(), "workload identity verifier") {
		t.Fatalf("production missing verifier was not rejected: %v", err)
	}
}

func testConfig(t *testing.T, key string) config.Config {
	t.Helper()
	return config.Config{
		Storage:          config.StorageConfig{Backend: "local", Root: t.TempDir(), Buckets: []string{"problems"}},
		WorkloadIdentity: config.WorkloadIdentityConfig{PublicKeyPEM: key, KeyID: "workload-1"},
	}
}
