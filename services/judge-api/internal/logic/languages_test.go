package logic

import (
	"testing"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/svc"
)

func TestNormalizeLanguageID(t *testing.T) {
	tests := map[string]string{
		" cpp17 ": "cpp17",
		"C++":     "cpp17",
		"c":       "c11",
		"PY3":     "python3",
		"python":  "python3",
		"java":    "java17",
		"rust":    "rust",
	}

	for input, want := range tests {
		t.Run(input, func(t *testing.T) {
			if got := normalizeLanguageID(input); got != want {
				t.Fatalf("normalizeLanguageID(%q) = %q, want %q", input, got, want)
			}
		})
	}
}

func TestValidateEnabledLanguage(t *testing.T) {
	svcCtx := &svc.ServiceContext{
		Config: config.Config{
			Languages: config.LanguagesConfig{
				Items: []config.LanguageConfig{
					{Id: "cpp17", DisplayName: "C++17", Version: "GCC", Enabled: true},
					{Id: "python3", DisplayName: "Python 3", Version: "CPython", Enabled: false},
				},
			},
		},
	}

	got, err := validateEnabledLanguage(svcCtx, "C++")
	if err != nil {
		t.Fatalf("expected cpp17 to be accepted: %v", err)
	}
	if got != "cpp17" {
		t.Fatalf("expected cpp17, got %q", got)
	}

	if _, err := validateEnabledLanguage(svcCtx, "python"); err == nil {
		t.Fatalf("expected disabled python3 to be rejected")
	}

	if _, err := validateEnabledLanguage(svcCtx, "rust"); err == nil {
		t.Fatalf("expected unsupported language to be rejected")
	}
}

func TestMaxCodeBytesFallback(t *testing.T) {
	if got := maxCodeBytes(&svc.ServiceContext{}); got != defaultMaxCodeBytes {
		t.Fatalf("expected default max code bytes, got %d", got)
	}

	svcCtx := &svc.ServiceContext{
		Config: config.Config{
			Submission: config.SubmissionConfig{MaxCodeBytes: 1234},
		},
	}
	if got := maxCodeBytes(svcCtx); got != 1234 {
		t.Fatalf("expected configured max code bytes, got %d", got)
	}
}
