package logic

import (
	"testing"

	"ojos-problem-service/internal/types"
)

func TestNormalizeProblemNo(t *testing.T) {
	if got, err := normalizeProblemNo("P1001"); err != nil || got != "P1001" {
		t.Fatalf("expected valid problem number, got %q err=%v", got, err)
	}
	if _, err := normalizeProblemNo("bad space"); err == nil {
		t.Fatalf("expected invalid problem number")
	}
}

func TestNormalizeLanguageLimitsAppliesDefaultsAndOverrides(t *testing.T) {
	repoLimits, packageLimits, err := normalizeLanguageLimits([]types.ProblemLanguageLimit{
		{Language: "python3", TimeLimitMs: 5000, MemoryLimitMb: 768},
		{Language: "rust", TimeLimitMs: 1200, MemoryLimitMb: 256},
	}, 1000, 256)
	if err != nil {
		t.Fatal(err)
	}

	repoByLanguage := map[string]types.ProblemLanguageLimit{}
	for _, limit := range repoLimits {
		repoByLanguage[limit.Language] = types.ProblemLanguageLimit{
			Language:      limit.Language,
			TimeLimitMs:   limit.TimeLimitMs,
			MemoryLimitMb: limit.MemoryLimitMb,
		}
	}
	if repoByLanguage["cpp17"].TimeLimitMs != 1000 {
		t.Fatalf("cpp17 default not applied: %#v", repoByLanguage)
	}
	if repoByLanguage["java17"].MemoryLimitMb != 512 {
		t.Fatalf("java17 special memory default not applied: %#v", repoByLanguage["java17"])
	}
	if repoByLanguage["python3"].TimeLimitMs != 5000 || repoByLanguage["python3"].MemoryLimitMb != 768 {
		t.Fatalf("python3 override not applied: %#v", repoByLanguage["python3"])
	}
	if repoByLanguage["rust"].TimeLimitMs != 1200 {
		t.Fatalf("rust override missing: %#v", repoByLanguage)
	}

	packageByLanguage := map[string]int{}
	for _, limit := range packageLimits {
		packageByLanguage[limit.Language] = limit.TimeMs
	}
	if packageByLanguage["python3"] != 5000 || packageByLanguage["rust"] != 1200 {
		t.Fatalf("package limits mismatch: %#v", packageByLanguage)
	}
}

func TestNormalizeComponentsAcceptsCustomComponent(t *testing.T) {
	components, err := normalizeComponents(
		types.ProblemComponentInput{
			Type:       "custom",
			Name:       "special-runner",
			Language:   "python3",
			SourcePath: "runner/special.py",
			SourceCode: "print('ok')\n",
			Args:       []string{"", "--strict"},
		},
		types.ProblemComponentInput{},
		types.ProblemComponentInput{},
		types.ProblemComponentInput{},
	)
	if err != nil {
		t.Fatal(err)
	}
	if components.Runner.Type != "custom" || components.Runner.Name != "special-runner" {
		t.Fatalf("runner mismatch: %#v", components.Runner)
	}
	if len(components.Runner.Args) != 1 || components.Runner.Args[0] != "--strict" {
		t.Fatalf("args not compacted: %#v", components.Runner.Args)
	}
}

func TestNormalizeComponentsRejectsPathTraversal(t *testing.T) {
	_, err := normalizeComponents(
		types.ProblemComponentInput{
			Type:       "custom",
			Name:       "bad-runner",
			SourcePath: "../runner.cpp",
		},
		types.ProblemComponentInput{},
		types.ProblemComponentInput{},
		types.ProblemComponentInput{},
	)
	if err == nil {
		t.Fatalf("expected path traversal to be rejected")
	}
}
