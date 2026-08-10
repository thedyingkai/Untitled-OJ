package logic

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
)

func TestArtifactRefForProblemSnapshotUsesImmutableStorageObject(t *testing.T) {
	svcCtx := &svc.ServiceContext{Config: config.Config{Storage: config.StorageConfig{
		InternalGatewayEndpoint: "http://gateway:8080",
		GetApiID:                "storage.object.get",
	}, WorkloadIdentity: config.WorkloadIdentityConfig{AllowLegacyWorkerToken: true}}}
	submission := &repository.SubmissionView{
		ProblemArtifactURI:       "storage://problems/package-sha256-abc.zip",
		ProblemArtifactSHA256:    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
		ProblemArtifactSizeBytes: 123,
	}
	ref, err := artifactRefForProblemSnapshot(svcCtx, submission, "/legacy")
	if err != nil {
		t.Fatal(err)
	}
	if ref.Url != "/internal/apis/storage.object.get/problems/package-sha256-abc.zip" {
		t.Fatalf("unexpected immutable artifact URL: %s", ref.Url)
	}
	if ref.Binding != "storage_get" || ref.ApiId != "storage.object.get" || ref.RelativePath != "/problems/package-sha256-abc.zip" {
		t.Fatalf("artifact did not include a stable storage binding reference: %#v", ref)
	}
	if ref.Sha256 != "sha256:"+submission.ProblemArtifactSHA256 || ref.SizeBytes != submission.ProblemArtifactSizeBytes {
		t.Fatalf("artifact identity was not preserved: %#v", ref)
	}
}

func TestProductionArtifactRefWithoutManagedJudgeContextStillContainsNoURL(t *testing.T) {
	svcCtx := &svc.ServiceContext{Config: config.Config{Storage: config.StorageConfig{
		InternalGatewayEndpoint: "http://gateway:8080",
		GetApiID:                "storage.object.get",
	}}}
	submission := &repository.SubmissionView{
		ProblemArtifactURI:       "storage://problems/package-sha256-production.zip",
		ProblemArtifactSHA256:    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
		ProblemArtifactSizeBytes: 123,
	}
	ref, err := artifactRefForProblemSnapshot(svcCtx, submission, "/legacy")
	if err != nil {
		t.Fatal(err)
	}
	if ref.Url != "" {
		t.Fatalf("production task leaked a topology-dependent URL: %q", ref.Url)
	}
	if ref.Binding != "storage_get" || ref.RelativePath != "/problems/package-sha256-production.zip" {
		t.Fatalf("production task did not contain a stable ApiResourceRef: %#v", ref)
	}
	encoded, err := json.Marshal(ref)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(encoded), `"url"`) {
		t.Fatalf("production ApiResourceRef still serialized the retired URL field: %s", encoded)
	}
}

func TestManagedArtifactRefContainsNoURLAndSurvivesGatewayBindingChanges(t *testing.T) {
	root := t.TempDir()
	tokenPath := filepath.Join(root, "token")
	contextPath := filepath.Join(root, "context.json")
	if err := os.WriteFile(tokenPath, []byte("deployment-token"), 0o600); err != nil {
		t.Fatal(err)
	}
	contextJSON, err := json.Marshal(map[string]any{
		"schema_version": 1,
		"deployment":     map[string]any{"id": "judge-a", "service": "judge-api", "node": "node-a"},
		"gateway":        map[string]any{"origin": "https://gateway-a.example"},
		"bindings": map[string]any{
			"storage_get":  map[string]any{"binding_id": "get", "api_id": "storage.object.get", "base_path": "/internal/apis/storage.object.get", "timeout_ms": 300000},
			"storage_put":  map[string]any{"binding_id": "put", "api_id": "storage.object.put", "base_path": "/internal/apis/storage.object.put", "timeout_ms": 300000},
			"storage_head": map[string]any{"binding_id": "head", "api_id": "storage.object.head", "base_path": "/internal/apis/storage.object.head", "timeout_ms": 300000},
		},
		"credential_file": tokenPath,
		"generation":      7,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(contextPath, contextJSON, 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_SERVICE_CONTEXT_FILE", contextPath)

	submission := &repository.SubmissionView{
		ProblemArtifactURI:       "storage://problems/package-sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.zip",
		ProblemArtifactSHA256:    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
		ProblemArtifactSizeBytes: 123,
	}
	ref, err := artifactRefForProblemSnapshot(&svc.ServiceContext{}, submission, "/legacy")
	if err != nil {
		t.Fatal(err)
	}
	if ref.Url != "" {
		t.Fatalf("managed task leaked a topology-dependent URL: %q", ref.Url)
	}
	if ref.Binding != "storage_get" || ref.ApiId != "storage.object.get" || ref.RelativePath != "/problems/package-sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.zip" {
		t.Fatalf("managed task did not contain an ApiResourceRef: %#v", ref)
	}
}
