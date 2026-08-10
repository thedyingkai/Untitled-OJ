package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"ojos-auth-service/internal/config"
	"ojos-auth-service/internal/svc"
	atopology "ojos-auth-service/internal/topologyprojection"
	shared "ojos-shared/topologyprojection"

	"github.com/zeromicro/go-zero/rest/pathvar"
)

func TestAuthTopologyDeleteAcceptsOrchestratorNullSpec(t *testing.T) {
	payload := []byte(`{"api_version":"v1","provider":"auth","action":"delete","topology_id":"primary","attempted_revision_id":"primary:r1:abc","desired_revision_id":null,"desired_content_sha256":null,"operation_id":"operation-1","spec":null,"routes":[],"grants":[]}`)
	request := httptest.NewRequest(http.MethodDelete, "/api/v1/topologies/primary", bytes.NewReader(payload))
	request.Header.Set("Content-Type", "application/json")

	decoded, err := readTopologyRequest(request)
	if err != nil {
		t.Fatalf("decode Auth topology delete: %v", err)
	}
	if err := decoded.Validate("auth", "primary"); err != nil {
		t.Fatalf("Auth rejected Orchestrator's absent projection: %v", err)
	}
}

func TestAuthTopologyStatusReportsEffectiveProjectionDigest(t *testing.T) {
	revision := "revision-1"
	contentSHA256 := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	request := shared.Request{
		APIVersion: shared.APIVersion, Provider: "auth", Action: "apply",
		TopologyID: "primary", AttemptedRevisionID: revision,
		DesiredRevisionID: &revision, DesiredContentSHA256: &contentSHA256,
		OperationID: "operation-1",
		Spec:        json.RawMessage(`{"topology_id":"primary","endpoints":[],"links":[]}`),
		Routes:      []shared.BindingRoute{}, Grants: []shared.BindingGrant{},
	}
	store := atopology.NewStore(nil)
	if err := store.Apply(context.Background(), request); err != nil {
		t.Fatal(err)
	}
	serviceContext := &svc.ServiceContext{
		Config:             config.Config{InternalAuth: config.InternalAuthConfig{Token: "management-token"}},
		TopologyProjection: store,
	}

	httpRequest := httptest.NewRequest(http.MethodGet, "/api/v1/topologies/primary", nil)
	httpRequest.Header.Set("Authorization", "Bearer management-token")
	httpRequest = pathvar.WithVars(httpRequest, map[string]string{"id": "primary"})
	recorder := httptest.NewRecorder()
	topologyProjectionHandler(serviceContext).ServeHTTP(recorder, httpRequest)
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d, body = %s", recorder.Code, recorder.Body.String())
	}
	var body map[string]any
	if err := json.Unmarshal(recorder.Body.Bytes(), &body); err != nil {
		t.Fatal(err)
	}
	if body["observed_projection_sha256"] != "fa9d28278a0d02b19bfebeae5afd5aa6dde1c685d8396acc8defe8832848865c" {
		t.Fatalf("Auth status projection digest missing or wrong: %s", recorder.Body.String())
	}
}
