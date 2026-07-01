package logic

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/types"
)

func TestValidateWorkerStatus(t *testing.T) {
	for _, status := range []string{
		"ACCEPTED",
		"WRONG_ANSWER",
		"COMPILE_ERROR",
		"RUNTIME_ERROR",
		"TIME_LIMIT_EXCEEDED",
		"MEMORY_LIMIT_EXCEEDED",
		"OUTPUT_LIMIT_EXCEEDED",
		"SYSTEM_ERROR",
		"CANCELLED",
		"UNSUPPORTED_LANGUAGE",
	} {
		if err := validateWorkerStatus(status); err != nil {
			t.Fatalf("expected %s to be valid: %v", status, err)
		}
	}

	if err := validateWorkerStatus("PENDING"); err == nil {
		t.Fatalf("expected non-terminal status to be rejected")
	}
}

func TestWorkerBasePath(t *testing.T) {
	if got := workerBasePath(""); got != "/judge/worker" {
		t.Fatalf("expected default worker path, got %q", got)
	}
	if got := workerBasePath(" /internal/worker/ "); got != "/internal/worker" {
		t.Fatalf("expected trimmed worker path, got %q", got)
	}
}

func TestWorkerFailureResultEventBuildsTerminalResultPayload(t *testing.T) {
	req := &types.WorkerFailTaskReq{
		TaskId:       "sub-42",
		WorkerId:     "worker-a",
		LeaseVersion: 3,
		ErrorType:    "SYSTEM",
		Message:      "sandbox failed",
	}

	result := workerFailureResultEvent(req, "SYSTEM_ERROR", "sandbox failed")

	if result.TaskId != "sub-42" || result.WorkerId != "worker-a" || result.LeaseVersion != 3 {
		t.Fatalf("unexpected task identity in result event: %#v", result)
	}
	if result.Status != "SYSTEM_ERROR" || result.Message != "sandbox failed" {
		t.Fatalf("unexpected failure result summary: %#v", result)
	}
	if result.Score != 0 || result.TimeMs != 0 || result.MemoryKb != 0 || len(result.Cases) != 0 {
		t.Fatalf("failure result event should not invent metrics/cases: %#v", result)
	}
}

func TestWriteWorkerResultArtifactsTruncatesLogsAndWritesResult(t *testing.T) {
	resultPath := filepath.Join(t.TempDir(), "submission", "result.json")
	if err := os.MkdirAll(filepath.Dir(resultPath), 0755); err != nil {
		t.Fatal(err)
	}

	longStdout := strings.Repeat("x", maxWorkerLogBytes+1024)
	req := &types.WorkerSubmitResultReq{
		Status:   "OUTPUT_LIMIT_EXCEEDED",
		Score:    0,
		TimeMs:   12,
		MemoryKb: 2048,
		Cases: []types.WorkerResultCase{
			{
				CaseNo:     1,
				Status:     "OUTPUT_LIMIT_EXCEEDED",
				Score:      0,
				TimeMs:     12,
				MemoryKb:   2048,
				Stdout:     longStdout,
				Stderr:     "stderr",
				CheckerLog: "checker",
				Message:    "output too large",
			},
		},
	}

	err := writeWorkerResultArtifacts(context.Background(), config.StorageConfig{}, &repository.SubmissionView{ID: 99, ResultPath: resultPath}, req)
	if err != nil {
		t.Fatalf("writeWorkerResultArtifacts returned error: %v", err)
	}

	data, err := os.ReadFile(resultPath)
	if err != nil {
		t.Fatal(err)
	}

	var result ResultFile
	if err := json.Unmarshal(data, &result); err != nil {
		t.Fatal(err)
	}
	if result.SubmissionID != 99 || result.Status != "OUTPUT_LIMIT_EXCEEDED" {
		t.Fatalf("unexpected result summary: %#v", result)
	}
	if len(result.Cases) != 1 {
		t.Fatalf("expected one case, got %d", len(result.Cases))
	}

	stdoutData, err := os.ReadFile(filepath.FromSlash(result.Cases[0].StdoutPath))
	if err != nil {
		t.Fatal(err)
	}
	if len(stdoutData) != maxWorkerLogBytes {
		t.Fatalf("expected stdout to be truncated to %d bytes, got %d", maxWorkerLogBytes, len(stdoutData))
	}

	cases, err := readResultCases(resultPath)
	if err != nil {
		t.Fatalf("readResultCases returned error: %v", err)
	}
	if len(cases) != 1 {
		t.Fatalf("expected one public case item, got %d", len(cases))
	}
	if cases[0].MemoryKb != 2048 || cases[0].Status != "OUTPUT_LIMIT_EXCEEDED" {
		t.Fatalf("unexpected public case item: %#v", cases[0])
	}
}
