package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
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
