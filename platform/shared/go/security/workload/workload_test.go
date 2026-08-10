package workload

import (
	"crypto/ed25519"
	"crypto/rand"
	"testing"
	"time"
)

func TestIssueVerifyRoundTrip(t *testing.T) {
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	issuer, err := NewIssuer(privateKey, "workload-1", "issuer", "gateway", 15*time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	verifier, err := NewVerifier(publicKey, "workload-1", "issuer", "gateway")
	if err != nil {
		t.Fatal(err)
	}
	now := time.Unix(1_800_000_000, 0).UTC()
	token, expiresAt, err := issuer.Issue(IssueRequest{
		DeploymentID:         "deployment-b",
		ServiceID:            "judge-worker",
		NodeID:               "node-b",
		CredentialGeneration: 3,
	}, now)
	if err != nil {
		t.Fatal(err)
	}
	claims, err := verifier.Verify(token, now.Add(time.Minute))
	if err != nil {
		t.Fatal(err)
	}
	if claims.DeploymentID != "deployment-b" || claims.NodeID != "node-b" || claims.CredentialGeneration != 3 {
		t.Fatalf("unexpected claims: %#v", claims)
	}
	if !expiresAt.Equal(now.Add(15 * time.Minute)) {
		t.Fatalf("unexpected expiry: %s", expiresAt)
	}
}

func TestWrongAudienceAndExpiryAreRejected(t *testing.T) {
	publicKey, privateKey, _ := ed25519.GenerateKey(rand.Reader)
	issuer, _ := NewIssuer(privateKey, "workload-1", "issuer", "gateway", time.Minute)
	wrongAudience, _ := NewVerifier(publicKey, "workload-1", "issuer", "other")
	verifier, _ := NewVerifier(publicKey, "workload-1", "issuer", "gateway")
	now := time.Unix(1_800_000_000, 0).UTC()
	token, _, err := issuer.Issue(IssueRequest{
		DeploymentID:         "deployment-b",
		ServiceID:            "judge-worker",
		NodeID:               "node-b",
		CredentialGeneration: 1,
	}, now)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := wrongAudience.Verify(token, now); err == nil {
		t.Fatal("wrong audience must be rejected")
	}
	if _, err := verifier.Verify(token, now.Add(2*time.Minute)); err == nil {
		t.Fatal("expired token must be rejected")
	}
}

func TestIssuerVerifierUsesIdenticalClaimPolicy(t *testing.T) {
	_, privateKey, _ := ed25519.GenerateKey(rand.Reader)
	issuer, err := NewIssuer(privateKey, "workload-1", "issuer", "gateway", time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Unix(1_800_000_000, 0).UTC()
	token, _, err := issuer.Issue(IssueRequest{
		DeploymentID: "deployment-a", ServiceID: "problem-service", NodeID: "node-a", CredentialGeneration: 4,
	}, now)
	if err != nil {
		t.Fatal(err)
	}
	claims, err := issuer.Verifier().Verify(token, now.Add(time.Second))
	if err != nil || claims.CredentialGeneration != 4 {
		t.Fatalf("issuer-derived verifier rejected its token: claims=%#v err=%v", claims, err)
	}
}
