package packagefs

import (
	"os"
	"path/filepath"
	"testing"
)

func TestValidatePackageAcceptsABPackage(t *testing.T) {
	packageDir := newTestPackage(t)

	validation, err := ValidatePackage(packageDir)
	if err != nil {
		t.Fatalf("ValidatePackage returned error: %v", err)
	}
	if !validation.Valid {
		t.Fatalf("expected package to be valid, errors: %#v", validation.Errors)
	}
}

func TestValidatePackageRejectsBrokenCases(t *testing.T) {
	tests := []struct {
		name string
		edit func(t *testing.T, packageDir string)
		code string
	}{
		{
			name: "missing input",
			edit: func(t *testing.T, packageDir string) {
				t.Helper()
				if err := os.Remove(filepath.Join(packageDir, "tests", "001.in")); err != nil {
					t.Fatal(err)
				}
			},
			code: "missing_input",
		},
		{
			name: "missing answer",
			edit: func(t *testing.T, packageDir string) {
				t.Helper()
				if err := os.Remove(filepath.Join(packageDir, "tests", "001.ans")); err != nil {
					t.Fatal(err)
				}
			},
			code: "missing_answer",
		},
		{
			name: "negative score",
			edit: func(t *testing.T, packageDir string) {
				t.Helper()
				writeCases(t, packageDir, `
cases:
  - case_no: 1
    input: 001.in
    answer: 001.ans
    score: -1
    group: 0
`)
			},
			code: "invalid_score",
		},
		{
			name: "invalid yaml",
			edit: func(t *testing.T, packageDir string) {
				t.Helper()
				writeCases(t, packageDir, "cases:\n  - : bad\n")
			},
			code: "invalid_yaml",
		},
		{
			name: "path traversal",
			edit: func(t *testing.T, packageDir string) {
				t.Helper()
				writeCases(t, packageDir, `
cases:
  - case_no: 1
    input: ../secret.txt
    answer: 001.ans
    score: 100
    group: 0
`)
			},
			code: "path_escape",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			packageDir := newTestPackage(t)
			tt.edit(t, packageDir)

			validation, err := ValidatePackage(packageDir)
			if err != nil {
				t.Fatalf("ValidatePackage returned error: %v", err)
			}
			if validation.Valid {
				t.Fatalf("expected validation failure")
			}
			if !hasValidationCode(validation.Errors, tt.code) {
				t.Fatalf("expected error code %q, got %#v", tt.code, validation.Errors)
			}
		})
	}
}

func newTestPackage(t *testing.T) string {
	t.Helper()

	result, err := CreateInitialPackage(CreateProblemArgs{
		Root:          t.TempDir(),
		ID:            1,
		Slug:          "a-plus-b",
		Title:         "A+B",
		Statement:     "Add two integers.",
		ProblemType:   "traditional",
		Visibility:    "public",
		TimeLimitMs:   1000,
		MemoryLimitMb: 256,
	})
	if err != nil {
		t.Fatal(err)
	}

	if _, err := AddCase(AddCaseArgs{
		PackageDir: result.PackageDir,
		CaseNo:     1,
		Input:      "1 2\n",
		Answer:     "3\n",
		Score:      100,
		Group:      0,
		Sample:     true,
		Hidden:     false,
	}); err != nil {
		t.Fatal(err)
	}

	return result.PackageDir
}

func writeCases(t *testing.T, packageDir string, content string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(packageDir, "tests", "cases.yaml"), []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
}

func hasValidationCode(issues []PackageValidationIssue, code string) bool {
	for _, issue := range issues {
		if issue.Code == code {
			return true
		}
	}
	return false
}
