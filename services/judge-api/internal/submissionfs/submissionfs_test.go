package submissionfs

import (
	"os"
	"path/filepath"
	"testing"
)

func TestCreateSubmissionFilesCleansPreviousArtifacts(t *testing.T) {
	root := t.TempDir()

	first, err := CreateSubmissionFiles(CreateSubmissionFilesArgs{
		Root:         root,
		SubmissionID: 7,
		Language:     "python3",
		SourceFile:   "main.py",
		Code:         "print(3)\n",
	})
	if err != nil {
		t.Fatalf("create python submission files: %v", err)
	}

	if err := os.WriteFile(filepath.Join(root, "7", "build", "old.o"), []byte("old"), 0o644); err != nil {
		t.Fatalf("write old build artifact: %v", err)
	}
	if err := os.WriteFile(first.ResultPath, []byte(`{"status":"OLD"}`), 0o644); err != nil {
		t.Fatalf("write old result: %v", err)
	}

	second, err := CreateSubmissionFiles(CreateSubmissionFilesArgs{
		Root:         root,
		SubmissionID: 7,
		Language:     "java17",
		SourceFile:   "Main.java",
		Code:         "public class Main {}\n",
	})
	if err != nil {
		t.Fatalf("create java submission files: %v", err)
	}

	if _, err := os.Stat(filepath.Join(root, "7", "source", "main.py")); !os.IsNotExist(err) {
		t.Fatalf("old python source should be removed, stat err=%v", err)
	}
	if _, err := os.Stat(filepath.Join(root, "7", "build", "old.o")); !os.IsNotExist(err) {
		t.Fatalf("old build artifact should be removed, stat err=%v", err)
	}
	if _, err := os.Stat(second.CodePath); err != nil {
		t.Fatalf("new java source missing: %v", err)
	}

	result, err := os.ReadFile(second.ResultPath)
	if err != nil {
		t.Fatalf("read new result file: %v", err)
	}
	if string(result) != "{\"cases\":[]}\n" {
		t.Fatalf("result file was not reset, got %q", string(result))
	}
}
