package svc

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"

	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/proxy"
)

func TestContributionSnapshotAcknowledgementRetriesWithoutReapplyingUnchangedSnapshot(t *testing.T) {
	digest := "sha256:" + strings.Repeat("a", 64)
	revision := "sha256:" + strings.Repeat("b", 64)
	obligation := orchestratorsnapshot.ContributionAcknowledgement{
		ActivationID: "activation-1", ServiceID: "contest", CandidateRevisionID: revision,
		CandidateGeneration: 2, ExpectedState: "ACTIVE", ObservedRevisionID: &revision,
	}
	generation := uint64(2)
	obligation.ObservedGeneration = &generation
	snapshot := orchestratorsnapshot.ContributionSnapshot{
		SchemaVersion: "ojos.dev/contribution-snapshot/v1", Digest: digest, ScopeID: "default",
		Acknowledgements: []orchestratorsnapshot.ContributionAcknowledgement{obligation},
	}
	var mu sync.Mutex
	gets, posts := 0, 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		switch r.URL.Path {
		case "/api/v1/contributions/snapshot":
			gets++
			_ = json.NewEncoder(w).Encode(map[string]any{"data": snapshot, "meta": map[string]any{"api_version": "v1", "request_id": "get"}})
		case "/api/v1/contributions/projections:ack":
			posts++
			if r.Header.Get("x-ojos-orchestrator-token") != "internal" || r.Header.Get("x-ojos-contribution-ack-token") != "ack" || r.Header.Get("Idempotency-Key") != "contribution-projection-ack:GATEWAY:"+digest {
				t.Errorf("wrong acknowledgement headers: %v", r.Header)
			}
			if posts == 1 {
				http.Error(w, "retry", http.StatusServiceUnavailable)
				return
			}
			w.Header().Set("Content-Type", "application/json")
			_ = json.NewEncoder(w).Encode(map[string]any{"data": map[string]any{
				"schema_version": "ojos.dev/contribution-projection-ack/v1", "target": "GATEWAY", "scope_id": "default", "snapshot_digest": digest, "accepted": true,
			}, "meta": map[string]any{"api_version": "v1", "request_id": "ack"}})
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()
	serviceProxy, err := proxy.NewServiceProxy(nil, nil, "secret", nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	ctx := &ServiceContext{
		Orchestrator: orchestratorsnapshot.NewClient(server.URL, "internal", "ack"),
		ServiceProxy: serviceProxy,
	}
	if err := ctx.reloadContributionSnapshot(context.Background()); err == nil {
		t.Fatal("first acknowledgement failure was hidden")
	}
	if ctx.contributionDigest != digest || ctx.contributionPending == nil {
		t.Fatal("successful local apply was rolled back after acknowledgement failure")
	}
	if err := ctx.reloadContributionSnapshot(context.Background()); err != nil {
		t.Fatalf("retry unchanged acknowledgement: %v", err)
	}
	mu.Lock()
	defer mu.Unlock()
	if gets != 2 || posts != 2 || ctx.contributionPending != nil || ctx.contributionAcked != digest {
		t.Fatalf("unexpected retry state gets=%d posts=%d pending=%v acked=%q", gets, posts, ctx.contributionPending != nil, ctx.contributionAcked)
	}
}

func TestContributionSnapshotApplyFailureDoesNotAcknowledge(t *testing.T) {
	digest := "sha256:" + strings.Repeat("a", 64)
	posts := 0
	snapshot := orchestratorsnapshot.ContributionSnapshot{
		SchemaVersion: "ojos.dev/contribution-snapshot/v1", Digest: digest, ScopeID: "default",
		GatewayRoutes: []orchestratorsnapshot.ContributionGatewayRoute{
			{
				ServiceID: "contest", DeploymentID: "deployment", RevisionID: "bad", Generation: 1,
				Audience: "user", Method: "GET", Path: "/contests", ApiID: "contest.v1", OperationID: "list", ProviderPath: "/contests", Enabled: true,
			},
			{
				ServiceID: "contest", DeploymentID: "deployment", RevisionID: "bad", Generation: 1,
				Audience: "user", Method: "GET", Path: "/contests", ApiID: "contest.v1", OperationID: "duplicate", ProviderPath: "/contests", Enabled: true,
			},
		},
	}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost {
			posts++
			return
		}
		_ = json.NewEncoder(w).Encode(map[string]any{"data": snapshot, "meta": map[string]any{"api_version": "v1"}})
	}))
	defer server.Close()
	serviceProxy, err := proxy.NewServiceProxy(nil, nil, "secret", nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	ctx := &ServiceContext{Orchestrator: orchestratorsnapshot.NewClient(server.URL, "internal", "ack"), ServiceProxy: serviceProxy}
	if err := ctx.reloadContributionSnapshot(context.Background()); err == nil {
		t.Fatal("invalid route snapshot unexpectedly applied")
	}
	if posts != 0 {
		t.Fatalf("apply failure emitted %d acknowledgements", posts)
	}
	if _, err := servicestatus.ContributionRouteTable(snapshot); err == nil {
		t.Fatal("test fixture no longer fails route compilation")
	}
}
