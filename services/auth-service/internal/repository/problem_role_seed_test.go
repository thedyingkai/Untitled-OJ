package repository

import (
	"os"
	"strings"
	"testing"
)

func TestProblemSetterSeedIncludesCreateAndTestDataWrite(t *testing.T) {
	core, err := os.ReadFile("../../migrations/000011_grant_problem_setter_create.up.sql")
	if err != nil {
		t.Fatalf("read problem setter create migration: %v", err)
	}
	if !strings.Contains(string(core), "problem.create") || !strings.Contains(string(core), "problem_setter") {
		t.Fatalf("problem setter create migration must bind problem.create to problem_setter")
	}

	service, err := os.ReadFile("../../migrations/000010_seed_service_permissions.up.sql")
	if err != nil {
		t.Fatalf("read service permission seed migration: %v", err)
	}
	for _, want := range []string{"problem.testdata.write", "problem_setter"} {
		if !strings.Contains(string(service), want) {
			t.Fatalf("service permission seed migration missing %q", want)
		}
	}
}
