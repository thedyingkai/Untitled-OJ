package topologyprojection

import (
	"context"
	"encoding/json"
	"testing"

	shared "ojos-shared/topologyprojection"
)

func authRequest(topologyID, operationID, bindingID string) shared.Request {
	revision := "revision-1"
	hash := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	return shared.Request{
		APIVersion: shared.APIVersion, Provider: "auth", Action: "apply",
		TopologyID: topologyID, AttemptedRevisionID: revision,
		DesiredRevisionID: &revision, DesiredContentSHA256: &hash, OperationID: operationID,
		Spec: json.RawMessage(`{"topology_id":"` + topologyID + `","endpoints":[],"links":[]}`),
		Routes: []shared.BindingRoute{{
			BindingID: bindingID, RequirementName: "storage_get", ConsumerDeploymentID: "worker-b",
			ConsumerServiceID: "judge-worker", ConsumerNodeID: "node-b",
			CredentialGeneration: 2, APIID: "storage.object.get", ProviderDeploymentID: "storage-a",
			ProviderServiceID: "storage", ProviderNodeID: "node-a", ProviderEndpoint: "10.0.0.1:8080:storage",
			UpstreamBase: "https://10.0.0.1:8080", ProviderPath: "/objects",
			VirtualPath: "/internal/apis/storage.object.get", AuthMode: "workload", ProviderAuthMode: "workload",
			Permission: "storage.object.read", Methods: []string{"GET"}, TimeoutMS: 300000,
		}},
		Grants: []shared.BindingGrant{{
			BindingID: bindingID, RequirementName: "storage_get", ConsumerDeploymentID: "worker-b",
			ConsumerServiceID: "judge-worker", ConsumerNodeID: "node-b", CredentialGeneration: 2,
			APIID: "storage.object.get", Permission: "storage.object.read",
		}},
	}
}

func TestMemoryProjectionIsDurableForProcessAndIdempotent(t *testing.T) {
	ctx := context.Background()
	store := NewStore(nil)
	request := authRequest("primary", "operation-1", "binding-1")
	if err := request.Validate("auth", "primary"); err != nil {
		t.Fatal(err)
	}
	if err := store.Apply(ctx, request); err != nil {
		t.Fatal(err)
	}
	if err := store.Apply(ctx, request); err != nil {
		t.Fatalf("idempotent replay failed: %v", err)
	}
	document, err := store.Get(ctx, "primary")
	if err != nil || document == nil || len(document.Grants) != 1 {
		t.Fatalf("projection was not observable: document=%v err=%v", document, err)
	}
	if err := store.Delete(ctx, "primary"); err != nil {
		t.Fatal(err)
	}
	if document, _ := store.Get(ctx, "primary"); document != nil {
		t.Fatal("deleted projection remains observable")
	}
}

func TestMemoryProjectionRejectsCrossTopologyConsumerRequirement(t *testing.T) {
	ctx := context.Background()
	store := NewStore(nil)
	if err := store.Apply(ctx, authRequest("primary", "operation-1", "binding-1")); err != nil {
		t.Fatal(err)
	}
	if err := store.Apply(ctx, authRequest("secondary", "operation-2", "binding-2")); err == nil {
		t.Fatal("cross-topology duplicate consumer requirement was accepted")
	}
}

func TestAuthorizeWorkloadRequiresExactAppliedGrantAndRevokesOnDelete(t *testing.T) {
	ctx := context.Background()
	store := NewStore(nil)
	if err := store.Apply(ctx, authRequest("primary", "operation-1", "binding-1")); err != nil {
		t.Fatal(err)
	}
	tests := []struct {
		name       string
		deployment string
		service    string
		node       string
		generation uint64
		api        string
		permission string
		want       bool
	}{
		{name: "exact", deployment: "worker-b", service: "judge-worker", node: "node-b", generation: 2, api: "storage.object.get", permission: "storage.object.read", want: true},
		{name: "deployment", deployment: "other", service: "judge-worker", node: "node-b", generation: 2, api: "storage.object.get", permission: "storage.object.read"},
		{name: "service", deployment: "worker-b", service: "problem-service", node: "node-b", generation: 2, api: "storage.object.get", permission: "storage.object.read"},
		{name: "node", deployment: "worker-b", service: "judge-worker", node: "node-a", generation: 2, api: "storage.object.get", permission: "storage.object.read"},
		{name: "generation", deployment: "worker-b", service: "judge-worker", node: "node-b", generation: 1, api: "storage.object.get", permission: "storage.object.read"},
		{name: "api", deployment: "worker-b", service: "judge-worker", node: "node-b", generation: 2, api: "storage.object.put", permission: "storage.object.read"},
		{name: "permission", deployment: "worker-b", service: "judge-worker", node: "node-b", generation: 2, api: "storage.object.get", permission: "storage.object.write"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			allowed, err := store.AuthorizeWorkload(ctx, test.deployment, test.service, test.node, test.generation, test.api, test.permission)
			if err != nil || allowed != test.want {
				t.Fatalf("allowed=%v want=%v err=%v", allowed, test.want, err)
			}
		})
	}
	if err := store.Delete(ctx, "primary"); err != nil {
		t.Fatal(err)
	}
	allowed, err := store.AuthorizeWorkload(ctx, "worker-b", "judge-worker", "node-b", 2, "storage.object.get", "storage.object.read")
	if err != nil || allowed {
		t.Fatalf("deleted/unlinked grant remained active: allowed=%v err=%v", allowed, err)
	}
}

func TestMemoryProjectionRestorePreviousIsCASAndIdempotent(t *testing.T) {
	ctx := context.Background()
	store := NewStore(nil)
	attempt := authRequest("primary", "operation-2", "binding-new")
	attemptRevision := "revision-2"
	attempt.AttemptedRevisionID = attemptRevision
	attempt.DesiredRevisionID = &attemptRevision
	if err := store.Apply(ctx, attempt); err != nil {
		t.Fatal(err)
	}

	previousRevision := "revision-1"
	restore := authRequest("primary", "operation-2", "binding-previous")
	restore.Action = "restore_previous"
	restore.AttemptedRevisionID = attemptRevision
	restore.DesiredRevisionID = &previousRevision
	if err := restore.Validate("auth", "primary"); err != nil {
		t.Fatal(err)
	}
	if err := store.Apply(ctx, restore); err != nil {
		t.Fatalf("restore failed: %v", err)
	}
	if err := store.Apply(ctx, restore); err != nil {
		t.Fatalf("restore replay failed: %v", err)
	}
	document, err := store.Get(ctx, "primary")
	if err != nil || document == nil || document.RevisionID != previousRevision || document.OperationID != "operation-2" || document.Grants[0].BindingID != "binding-previous" {
		t.Fatalf("unexpected restored document: document=%v err=%v", document, err)
	}

	tampered := restore
	tampered.Grants = append([]shared.BindingGrant(nil), restore.Grants...)
	tampered.Grants[0].Permission = "storage.object.write"
	if err := store.Apply(ctx, tampered); err == nil {
		t.Fatal("restore replay with different desired grants was accepted")
	}
	document, err = store.Get(ctx, "primary")
	if err != nil || document == nil || document.Grants[0].Permission != "storage.object.read" {
		t.Fatalf("rejected restore changed memory state: document=%v err=%v", document, err)
	}
}

func TestMemoryProjectionStaleRestoreCannotOverwriteNewOperation(t *testing.T) {
	ctx := context.Background()
	store := NewStore(nil)
	attemptRevision := "revision-2"
	attempt := authRequest("primary", "operation-2", "binding-2")
	attempt.AttemptedRevisionID = attemptRevision
	attempt.DesiredRevisionID = &attemptRevision
	if err := store.Apply(ctx, attempt); err != nil {
		t.Fatal(err)
	}

	newRevision := "revision-3"
	newer := authRequest("primary", "operation-3", "binding-3")
	newer.AttemptedRevisionID = newRevision
	newer.DesiredRevisionID = &newRevision
	if err := store.Apply(ctx, newer); err != nil {
		t.Fatal(err)
	}

	previousRevision := "revision-1"
	restore := authRequest("primary", "operation-2", "binding-1")
	restore.Action = "restore_previous"
	restore.AttemptedRevisionID = attemptRevision
	restore.DesiredRevisionID = &previousRevision
	if err := store.Apply(ctx, restore); err == nil {
		t.Fatal("stale restore was accepted over a newer operation")
	}
	document, err := store.Get(ctx, "primary")
	if err != nil || document == nil || document.RevisionID != newRevision || document.OperationID != "operation-3" {
		t.Fatalf("stale restore changed memory state: document=%v err=%v", document, err)
	}
}
