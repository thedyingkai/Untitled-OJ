package handler

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestOpenAPIOperationPermissionsMatchDomainLogic(t *testing.T) {
	serviceRoot := filepath.Join("..", "..")
	checks := []struct {
		file, operation, permission, logicFile, logicCheck string
	}{
		{"api/problem-read.openapi.yaml", "operationId: listProblems", "x-ojos-permission: problem.view", "internal/logic/listproblemslogic.go", `"problem.view"`},
		{"api/problem-read.openapi.yaml", "operationId: getProblem", "x-ojos-permission: problem.view", "internal/logic/getproblemlogic.go", `requiredPermission := "problem.view"`},
		{"api/problem-manage.openapi.yaml", "operationId: createProblem", "x-ojos-permission: problem.create", "internal/logic/createproblemlogic.go", `"problem.create"`},
		{"api/problem-manage.openapi.yaml", "operationId: updateProblem", "x-ojos-permission: problem.edit", "internal/logic/updateproblemlogic.go", `"problem.edit"`},
		{"api/problem-manage.openapi.yaml", "operationId: deleteProblem", "x-ojos-permission: problem.delete", "internal/logic/deleteproblemlogic.go", `"problem.delete"`},
		{"api/testdata-read.openapi.yaml", "operationId: getProblemPackage", "x-ojos-permission: problem.testdata.read", "internal/logic/getproblempackagelogic.go", "requireProblemDataPermission"},
		{"api/testdata-read.openapi.yaml", "operationId: listPackageCases", "x-ojos-permission: problem.testdata.read", "internal/logic/listpackagecaseslogic.go", "requireProblemDataPermission"},
		{"api/testdata-read.openapi.yaml", "operationId: listTestCases", "x-ojos-permission: problem.testdata.read", "internal/logic/listtestcaseslogic.go", `"problem.testdata.read"`},
		{"api/testdata-write.openapi.yaml", "operationId: addTestCase", "x-ojos-permission: problem.testdata.write", "internal/logic/addtestcaselogic.go", `"problem.testdata.write"`},
		{"api/testdata-write.openapi.yaml", "operationId: updateTestCase", "x-ojos-permission: problem.testdata.write", "internal/logic/updatetestcaselogic.go", `"problem.testdata.write"`},
		{"api/testdata-write.openapi.yaml", "operationId: deleteTestCase", "x-ojos-permission: problem.testdata.write", "internal/logic/deletetestcaselogic.go", `"problem.testdata.write"`},
	}
	for _, check := range checks {
		api, err := os.ReadFile(filepath.Join(serviceRoot, check.file))
		if err != nil {
			t.Fatal(err)
		}
		operationIndex := strings.Index(string(api), check.operation)
		if operationIndex < 0 || !strings.Contains(string(api)[operationIndex:], check.permission) {
			t.Fatalf("%s does not bind %s to %s", check.file, check.operation, check.permission)
		}
		logic, err := os.ReadFile(filepath.Join(serviceRoot, check.logicFile))
		if err != nil {
			t.Fatal(err)
		}
		if !strings.Contains(string(logic), check.logicCheck) {
			t.Fatalf("%s no longer proves %s", check.logicFile, check.logicCheck)
		}
	}
}
