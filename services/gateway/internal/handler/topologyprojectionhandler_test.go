package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/svc"
	gtopology "ojos-gateway/internal/topologyprojection"
	shared "ojos-shared/topologyprojection"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
	"github.com/zeromicro/go-zero/rest/pathvar"
)

func TestGatewayTopologyDeleteAcceptsOrchestratorNullSpec(t *testing.T) {
	payload := []byte(`{"api_version":"v1","provider":"gateway","action":"delete","topology_id":"primary","attempted_revision_id":"primary:r1:abc","desired_revision_id":null,"desired_content_sha256":null,"operation_id":"operation-1","spec":null,"routes":[],"grants":[]}`)
	request := httptest.NewRequest(http.MethodDelete, "/api/v1/topologies/primary", bytes.NewReader(payload))
	request.Header.Set("Content-Type", "application/json")

	decoded, err := readTopologyRequest(request)
	if err != nil {
		t.Fatalf("decode Gateway topology delete: %v", err)
	}
	if err := decoded.Validate("gateway", "primary"); err != nil {
		t.Fatalf("Gateway rejected Orchestrator's absent projection: %v", err)
	}
}

func TestGatewayTopologyStatusReportsEffectiveProjectionDigest(t *testing.T) {
	redisServer := miniredis.RunT(t)
	redisClient := redis.NewClient(&redis.Options{Addr: redisServer.Addr()})
	t.Cleanup(func() { _ = redisClient.Close() })

	revision := "revision-1"
	contentSHA256 := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	request := shared.Request{
		APIVersion: shared.APIVersion, Provider: "gateway", Action: "apply",
		TopologyID: "primary", AttemptedRevisionID: revision,
		DesiredRevisionID: &revision, DesiredContentSHA256: &contentSHA256,
		OperationID: "operation-1",
		Spec:        json.RawMessage(`{"topology_id":"primary","endpoints":[],"links":[]}`),
		Routes:      []shared.BindingRoute{}, Grants: []shared.BindingGrant{},
	}
	document := request.Document()
	payload, err := json.Marshal(document)
	if err != nil {
		t.Fatal(err)
	}
	if err := redisClient.Set(context.Background(), "ojos:gateway:topology-projection:v1:primary", payload, 0).Err(); err != nil {
		t.Fatal(err)
	}
	serviceContext := &svc.ServiceContext{
		Config: config.Config{Orchestrator: config.OrchestratorConfig{
			InternalToken: "generic-internal-token", ManagementToken: "management-token",
		}},
		TopologyProjection: gtopology.NewStore(redisClient, nil),
	}
	unauthorizedRequest := httptest.NewRequest(http.MethodGet, "/api/v1/topologies/primary", nil)
	unauthorizedRequest.Header.Set("Authorization", "Bearer generic-internal-token")
	unauthorizedRequest = pathvar.WithVars(unauthorizedRequest, map[string]string{"id": "primary"})
	unauthorizedRecorder := httptest.NewRecorder()
	topologyProjectionHandler(serviceContext).ServeHTTP(unauthorizedRecorder, unauthorizedRequest)
	if unauthorizedRecorder.Code != http.StatusUnauthorized {
		t.Fatalf("generic internal token reached topology provider: %d", unauthorizedRecorder.Code)
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
		t.Fatalf("Gateway status projection digest missing or wrong: %s", recorder.Body.String())
	}
}
