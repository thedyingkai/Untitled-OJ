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

func TestCreateInitialPackageStoresProblemContentAndLanguageLimits(t *testing.T) {
	result, err := CreateInitialPackage(CreateProblemArgs{
		Root:          t.TempDir(),
		ID:            42,
		ProblemNo:     "ABC-42",
		Slug:          "latex-sum",
		Title:         "LaTeX Sum",
		Statement:     "# Statement\n\nCompute $a+b$.\n\n$$a+b=c$$",
		Solution:      "# Solution\n\nUse integer addition.",
		ProblemType:   "traditional",
		Visibility:    "public",
		TimeLimitMs:   1000,
		MemoryLimitMb: 256,
		LanguageLimits: []LanguageLimit{
			{Language: "python3", TimeMs: 4500, MemoryMb: 768},
			{Language: "rust", TimeMs: 1200, MemoryMb: 256},
		},
	})
	if err != nil {
		t.Fatal(err)
	}

	var manifest ProblemManifest
	if err := readYAML(filepath.Join(result.PackageDir, "problem.yaml"), &manifest); err != nil {
		t.Fatal(err)
	}
	if manifest.ProblemNo != "ABC-42" {
		t.Fatalf("problem_no mismatch: %q", manifest.ProblemNo)
	}
	if manifest.Statement.Format != ContentFormatMarkdownLatex {
		t.Fatalf("statement format mismatch: %q", manifest.Statement.Format)
	}
	if manifest.Tutorial.Format != ContentFormatMarkdownLatex {
		t.Fatalf("tutorial format mismatch: %q", manifest.Tutorial.Format)
	}
	if manifest.Limits.Languages["python3"].TimeMs != 4500 || manifest.Limits.Languages["python3"].MemoryMb != 768 {
		t.Fatalf("python3 limits mismatch: %#v", manifest.Limits.Languages["python3"])
	}
	if manifest.Limits.Languages["rust"].TimeMs != 1200 {
		t.Fatalf("rust limit missing: %#v", manifest.Limits.Languages)
	}

	statement, err := os.ReadFile(filepath.Join(result.PackageDir, "statement", "zh-cn.md"))
	if err != nil {
		t.Fatal(err)
	}
	if string(statement) != "# Statement\n\nCompute $a+b$.\n\n$$a+b=c$$\n" {
		t.Fatalf("statement markdown changed: %q", string(statement))
	}

	solution, err := os.ReadFile(filepath.Join(result.PackageDir, "tutorial", "zh-cn.md"))
	if err != nil {
		t.Fatal(err)
	}
	if string(solution) != "# Solution\n\nUse integer addition.\n" {
		t.Fatalf("solution markdown changed: %q", string(solution))
	}
}

func TestCreateInitialPackageStoresCustomComponents(t *testing.T) {
	result, err := CreateInitialPackage(CreateProblemArgs{
		Root:          t.TempDir(),
		ID:            7,
		ProblemNo:     "CUSTOM-7",
		Slug:          "custom-components",
		Title:         "Custom Components",
		Statement:     "Use custom components.",
		ProblemType:   "interactive",
		Visibility:    "private",
		TimeLimitMs:   1000,
		MemoryLimitMb: 256,
		Components: ComponentSet{
			Runner: ComponentSpec{
				Type:       "custom",
				Name:       "two-process-runner",
				Language:   "python3",
				SourcePath: "runner/two_process_runner.py",
				SourceCode: "print('runner')\n",
				Args:       []string{"--pipe"},
			},
			Checker: ComponentSpec{
				Type:       "custom",
				Name:       "strict-checker",
				Language:   "cpp17",
				SourcePath: "checker/strict_checker.cpp",
				SourceCode: "int main(){return 0;}\n",
			},
			Validator: ComponentSpec{
				Type:       "custom",
				Name:       "input-validator",
				Language:   "cpp17",
				SourcePath: "validators/input_validator.cpp",
				SourceCode: "int main(){return 0;}\n",
			},
			Scorer: ComponentSpec{
				Type:       "custom",
				Name:       "partial-scorer",
				Language:   "python3",
				SourcePath: "scorer/partial_scorer.py",
				SourceCode: "print(100)\n",
			},
		},
	})
	if err != nil {
		t.Fatal(err)
	}

	validation, err := ValidatePackage(result.PackageDir)
	if err != nil {
		t.Fatal(err)
	}
	if !validation.Valid {
		t.Fatalf("expected custom component package to be valid, errors: %#v", validation.Errors)
	}

	var runner ComponentConfig
	if err := readYAML(filepath.Join(result.PackageDir, "runner", "runner.yaml"), &runner); err != nil {
		t.Fatal(err)
	}
	if runner.Type != "custom" || runner.Name != "two-process-runner" {
		t.Fatalf("runner config mismatch: %#v", runner)
	}
	if runner.Config["source"] != "runner/two_process_runner.py" {
		t.Fatalf("runner source mismatch: %#v", runner.Config)
	}
	if _, err := os.Stat(filepath.Join(result.PackageDir, "validators", "input_validator.cpp")); err != nil {
		t.Fatalf("validator source missing: %v", err)
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
