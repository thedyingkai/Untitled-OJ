package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ojos-shared/bootstrap"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/servicecontext"
)

func TestGeneratedContractCarriesReferenceCapabilities(t *testing.T) {
	bytes, err := os.ReadFile(filepath.Join("gen", "service.contract.json"))
	if err != nil {
		t.Fatal(err)
	}
	var contract struct {
		SchemaVersion  string                                               `json:"schemaVersion"`
		Operations     []struct{ OperationID, Audience, Permission string } `json:"operations"`
		ResourceClaims []struct{ Name, Type, Lifecycle string }             `json:"resourceClaims"`
		Migrations     []struct{ ID, Artifact, Resource string }            `json:"migrations"`
		Frontends      []struct{ Target string }                            `json:"frontends"`
		Events         struct {
			Publishes []struct{ Type, Delivery string } `json:"publishes"`
		} `json:"events"`
	}
	if err := json.Unmarshal(bytes, &contract); err != nil {
		t.Fatal(err)
	}
	if contract.SchemaVersion != "ojos.dev/service-contract/v3" || len(contract.Operations) != 8 {
		t.Fatalf("unexpected generated contract schema=%q operations=%d", contract.SchemaVersion, len(contract.Operations))
	}
	if len(contract.ResourceClaims) != 1 || contract.ResourceClaims[0].Type != "postgresql.database/v1" || contract.ResourceClaims[0].Lifecycle != "retain" {
		t.Fatalf("resource claim = %#v", contract.ResourceClaims)
	}
	if len(contract.Migrations) != 1 || contract.Migrations[0].Artifact != "contest-migration-v1" {
		t.Fatalf("migration = %#v", contract.Migrations)
	}
	if len(contract.Frontends) != 2 || len(contract.Events.Publishes) != 1 || contract.Events.Publishes[0].Delivery != "durable" {
		t.Fatalf("frontends=%d events=%#v", len(contract.Frontends), contract.Events.Publishes)
	}
	for _, operation := range contract.Operations {
		if operation.Audience != "internal" && strings.TrimSpace(operation.Permission) == "" {
			t.Fatalf("external operation %s lacks operation permission", operation.OperationID)
		}
	}
}

func TestManagedPermissionBootstrapUsesV3APIIDBinding(t *testing.T) {
	root := t.TempDir()
	credential := filepath.Join(root, "token")
	if err := os.WriteFile(credential, []byte("workload-token"), 0o600); err != nil {
		t.Fatal(err)
	}
	contextFile := filepath.Join(root, "context.json")
	document := servicecontext.ServiceContext{
		SchemaVersion: 1,
		Deployment: servicecontext.DeploymentIdentity{
			ID: "contest-a", Service: "contest-service", Node: "node-a",
		},
		Gateway: servicecontext.GatewayContext{Origin: "http://127.0.0.1:8080"},
		Bindings: map[string]servicecontext.APIBinding{
			sharedperm.DefaultPermissionCheckApiID: {
				BindingID: "binding-auth-permission",
				APIID:     sharedperm.DefaultPermissionCheckApiID,
				BasePath:  "/internal/apis/" + sharedperm.DefaultPermissionCheckApiID,
				TimeoutMS: 5_000,
			},
		},
		CredentialFile: credential,
		Generation:     1,
	}
	bytes, err := json.Marshal(document)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(contextFile, bytes, 0o600); err != nil {
		t.Fatal(err)
	}

	runtime, err := bootstrap.New(bootstrap.Manifest{
		Service: "contest-service",
		Components: []bootstrap.ComponentSpec{{
			Name: "permissions", Kind: bootstrap.KindPermission,
		}},
	}, bootstrap.Options{Factories: map[bootstrap.Kind]bootstrap.Factory{
		bootstrap.KindPermission: contestPermissionFactory(contextFile, true),
	}})
	if err != nil {
		t.Fatal(err)
	}
	if err := runtime.Start(context.Background()); err != nil {
		t.Fatalf("v3 API-ID binding did not start: %v", err)
	}
	if _, ok := runtime.Lookup(bootstrap.ValuePermissionChecker); !ok {
		t.Fatal("permission checker output was not published")
	}
	if err := runtime.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
}
