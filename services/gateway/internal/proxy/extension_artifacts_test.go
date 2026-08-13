package proxy

import (
	"context"
	"crypto/sha256"
	"crypto/tls"
	"encoding/hex"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"

	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
)

func artifactDigest(data []byte) string {
	sum := sha256.Sum256(data)
	return "sha256:" + hex.EncodeToString(sum[:])
}

func artifactSnapshot(reference string, digest string, enabled bool) orchestratorsnapshot.ContributionSnapshot {
	manifestDigest := "sha256:" + strings.Repeat("a", 64)
	manifestReference := "https://fixture.invalid/manifests/" + strings.TrimPrefix(manifestDigest, "sha256:") + "/manifest.json"
	return orchestratorsnapshot.ContributionSnapshot{
		SchemaVersion: "ojos.dev/contribution-snapshot/v1", Digest: "sha256:" + strings.Repeat("f", 64),
		UserFrontendModules: []orchestratorsnapshot.ContributionFrontendModule{{
			ServiceID: "contest-service", DeploymentID: "dep-1", RevisionID: "rev-1", Generation: 1,
			Target: "user-shell", ModuleID: "contest.user", Artifact: "bundle.js",
			ManifestDigest: manifestDigest, ManifestReference: manifestReference,
			BundleDigest: digest, BundleReference: reference, Enabled: enabled,
		}},
	}
}

func TestExtensionArtifactServesOnlyActiveVerifiedContent(t *testing.T) {
	bundle := []byte("export const value = 'verified';")
	digest := artifactDigest(bundle)
	var requests atomic.Int32
	source := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests.Add(1)
		_, _ = w.Write(bundle)
	}))
	defer source.Close()
	reference := source.URL + "/extensions/" + strings.TrimPrefix(digest, "sha256:") + "/bundle.js"

	rp := newTestServiceProxy(t, nil)
	rp.extensionArtifacts.client.Transport = source.Client().Transport
	if err := rp.SetContributionArtifacts(artifactSnapshot(reference, digest, true)); err != nil {
		t.Fatal(err)
	}
	path := extensionArtifactPrefix + "/" + strings.TrimPrefix(digest, "sha256:") + "/bundle.js"
	for requestNumber := 0; requestNumber < 2; requestNumber++ {
		rr := httptest.NewRecorder()
		rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, path, nil))
		if rr.Code != http.StatusOK || rr.Body.String() != string(bundle) || rr.Header().Get("Content-Type") != "text/javascript; charset=utf-8" {
			t.Fatalf("verified artifact response %d: status=%d body=%q headers=%v", requestNumber, rr.Code, rr.Body.String(), rr.Header())
		}
	}
	if requests.Load() != 1 {
		t.Fatalf("content-addressed artifact was downloaded %d times instead of cached", requests.Load())
	}

	if err := rp.SetContributionArtifacts(orchestratorsnapshot.ContributionSnapshot{}); err != nil {
		t.Fatal(err)
	}
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, path, nil))
	if rr.Code != http.StatusNotFound {
		t.Fatalf("retired artifact remained available: %d", rr.Code)
	}
}

func TestExtensionArtifactDigestMismatchFailsClosedAndIsNotCached(t *testing.T) {
	bundle := []byte("export const tampered = true;")
	expected := artifactDigest([]byte("expected bytes"))
	var requests atomic.Int32
	source := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests.Add(1)
		_, _ = w.Write(bundle)
	}))
	defer source.Close()
	reference := source.URL + "/extensions/" + strings.TrimPrefix(expected, "sha256:") + "/bundle.js"
	rp := newTestServiceProxy(t, nil)
	rp.extensionArtifacts.client.Transport = source.Client().Transport
	if err := rp.SetContributionArtifacts(artifactSnapshot(reference, expected, true)); err != nil {
		t.Fatal(err)
	}
	path := extensionArtifactPrefix + "/" + strings.TrimPrefix(expected, "sha256:") + "/bundle.js"
	for attempt := 0; attempt < 2; attempt++ {
		rr := httptest.NewRecorder()
		rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, path, nil))
		if rr.Code != http.StatusBadGateway || strings.Contains(rr.Body.String(), string(bundle)) {
			t.Fatalf("tampered artifact did not fail closed: status=%d body=%q", rr.Code, rr.Body.String())
		}
	}
	if requests.Load() != 2 {
		t.Fatalf("digest mismatch was cached after %d downloads", requests.Load())
	}
}

func TestExtensionArtifactRejectsUnknownInsecureAndRedirectedReferences(t *testing.T) {
	digest := "sha256:" + strings.Repeat("b", 64)
	if err := newExtensionArtifactRegistry().replace(artifactSnapshot("http://evil.invalid/"+strings.TrimPrefix(digest, "sha256:")+"/bundle.js", digest, true)); err == nil {
		t.Fatal("insecure artifact reference was accepted")
	}
	rp := newTestServiceProxy(t, nil)
	rr := httptest.NewRecorder()
	rp.ServeHTTP(rr, httptest.NewRequest(http.MethodGet, extensionArtifactPrefix+"/"+strings.TrimPrefix(digest, "sha256:")+"/bundle.js", nil))
	if rr.Code != http.StatusNotFound {
		t.Fatalf("unknown artifact should be 404, got %d", rr.Code)
	}

	redirectTarget := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, "should not be followed")
	}))
	defer redirectTarget.Close()
	redirect := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, redirectTarget.URL, http.StatusFound)
	}))
	defer redirect.Close()
	actual := artifactDigest([]byte("should not be followed"))
	reference := redirect.URL + "/extensions/" + strings.TrimPrefix(actual, "sha256:") + "/bundle.js"
	rp.extensionArtifacts.client.Transport = &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}} // test-only TLS fixtures
	if err := rp.SetContributionArtifacts(artifactSnapshot(reference, actual, true)); err != nil {
		t.Fatal(err)
	}
	_, err := rp.extensionArtifacts.load(context.Background(), extensionArtifactSpec{
		key: extensionArtifactKey{digest: actual, artifact: "bundle.js"}, reference: reference,
	})
	if err == nil || !strings.Contains(err.Error(), "302") {
		t.Fatalf("artifact redirect was followed or accepted: %v", err)
	}
}

func TestExtensionArtifactPathUsesHexWithoutDigestAlgorithmPrefix(t *testing.T) {
	digest := "sha256:" + strings.Repeat("a", 64)
	if _, _, ok := extensionArtifactRequest(extensionArtifactPrefix + "/" + digest + "/bundle.js"); ok {
		t.Fatal("artifact path accepted the sha256: algorithm prefix")
	}
	parsed, artifact, ok := extensionArtifactRequest(extensionArtifactPrefix + "/" + strings.TrimPrefix(digest, "sha256:") + "/bundle.js")
	if !ok || parsed != digest || artifact != "bundle.js" {
		t.Fatalf("hex artifact path did not restore canonical digest: digest=%q artifact=%q ok=%v", parsed, artifact, ok)
	}
}
