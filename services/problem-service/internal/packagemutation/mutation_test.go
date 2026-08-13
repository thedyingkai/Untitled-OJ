package packagemutation

import (
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"ojos-problem-events/problemv1"
	"ojos-problem-service/internal/packagefs"
	problemstorage "ojos-problem-service/internal/storage"
)

func TestReplaceRollbackRestoresByteExactLiveTree(t *testing.T) {
	root := t.TempDir()
	live := filepath.Join(root, "41-sum")
	writeTestTree(t, live, map[string]string{
		"problem.yaml":     "title: old\n",
		"tests/cases.yaml": "cases: []\n",
		"tests/001.in":     "1 2\n",
	})
	before := snapshotTestTree(t, live)

	workspace, err := newReplaceWorkspace(root, 41, live, 7)
	if err != nil {
		t.Fatal(err)
	}
	if err := workspace.cloneLiveTree(); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(workspace.stagingDir, "problem.yaml"), []byte("title: new\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if got := snapshotTestTree(t, live); !reflect.DeepEqual(got, before) {
		t.Fatal("staging mutation changed the live tree before commit")
	}
	artifact := testArtifact(t, workspace.stagingDir)
	if err := workspace.publishBeforeCommit(artifact, 8); err != nil {
		t.Fatal(err)
	}
	if string(mustReadFile(t, filepath.Join(live, "problem.yaml"))) != "title: new\n" {
		t.Fatal("prepared publish did not expose the staged tree")
	}
	journal, err := readJournal(workspace.journalPath, root, 41)
	if err != nil {
		t.Fatal(err)
	}
	// This is the filesystem half of an injected transaction/commit failure.
	if err := recoverFilesystem(journal, false); err != nil {
		t.Fatal(err)
	}
	after := snapshotTestTree(t, live)
	if !reflect.DeepEqual(after, before) {
		t.Fatalf("DB failure changed live package bytes: before=%v after=%v", before, after)
	}
	if _, err := os.Stat(workspace.backupDir); !os.IsNotExist(err) {
		t.Fatalf("rollback backup remains after restore: %v", err)
	}
}

func TestCreateAndDeleteJournalsRollbackFailClosed(t *testing.T) {
	root := t.TempDir()
	live := filepath.Join(root, "52-created")
	create, err := newCreateWorkspace(root, 52, live)
	if err != nil {
		t.Fatal(err)
	}
	writeTestTree(t, create.stagingDir, map[string]string{"problem.yaml": "title: staged\n"})
	artifact := testArtifact(t, create.stagingDir)
	if err := create.publishBeforeCommit(artifact, 1); err != nil {
		t.Fatal(err)
	}
	journal, err := readJournal(create.journalPath, root, 52)
	if err != nil {
		t.Fatal(err)
	}
	if err := recoverFilesystem(journal, false); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(live); !os.IsNotExist(err) {
		t.Fatalf("rolled-back create left a live tree: %v", err)
	}

	writeTestTree(t, live, map[string]string{"problem.yaml": "title: delete-me\n"})
	before := snapshotTestTree(t, live)
	remove, err := newDeleteWorkspace(root, 52, live, 3)
	if err != nil {
		t.Fatal(err)
	}
	if err := remove.publishDeleteBeforeCommit(artifact.SHA256, 4); err != nil {
		t.Fatal(err)
	}
	journal, err = readJournal(remove.journalPath, root, 52)
	if err != nil {
		t.Fatal(err)
	}
	if err := recoverFilesystem(journal, false); err != nil {
		t.Fatal(err)
	}
	if after := snapshotTestTree(t, live); !reflect.DeepEqual(after, before) {
		t.Fatalf("rolled-back delete did not restore bytes: before=%v after=%v", before, after)
	}
}

func TestCommittedJournalRejectsWrongLiveArtifact(t *testing.T) {
	root := t.TempDir()
	live := filepath.Join(root, "63-drift")
	writeTestTree(t, live, map[string]string{"problem.yaml": "title: expected\n"})
	artifact := testArtifact(t, live)
	journal := &mutationJournal{
		Operation:      "replace",
		LiveDir:        live,
		ArtifactSHA256: artifact.SHA256,
		Phase:          "live_published",
	}
	if err := os.WriteFile(filepath.Join(live, "problem.yaml"), []byte("title: drifted\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := recoverFilesystem(journal, true); !errors.Is(err, ErrRecoveryNeedsAttention) {
		t.Fatalf("committed recovery accepted an unproved live tree: %v", err)
	}
}

func TestRollbackJournalFailsClosedWhenOriginalTreeCannotBeProved(t *testing.T) {
	root := t.TempDir()
	journal := &mutationJournal{
		Operation:  "replace",
		LiveDir:    filepath.Join(root, "71-missing"),
		StagingDir: filepath.Join(root, ".71-missing.mutation-staging"),
		BackupDir:  filepath.Join(root, ".71-missing.mutation-backup"),
		Phase:      "prepared",
	}
	if err := recoverFilesystem(journal, false); !errors.Is(err, ErrRecoveryNeedsAttention) {
		t.Fatalf("rollback accepted a missing live tree and backup: %v", err)
	}
}

func TestValidateStagedPackageRejectsUndeclaredCaseGroup(t *testing.T) {
	result, err := packagefs.CreateInitialPackage(packagefs.CreateProblemArgs{
		Root:          t.TempDir(),
		ID:            81,
		Slug:          "invalid-staging-group",
		Title:         "Invalid staging group",
		ProblemType:   "traditional",
		Visibility:    "public",
		TimeLimitMs:   1000,
		MemoryLimitMb: 256,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(
		filepath.Join(result.PackageDir, "tests", "cases.yaml"),
		[]byte("cases:\n  - case_no: 1\n    input: 001.in\n    answer: 001.ans\n    score: 100\n    group: 1\n"),
		0o644,
	); err != nil {
		t.Fatal(err)
	}
	for name, content := range map[string]string{"001.in": "1 2\n", "001.ans": "3\n"} {
		if err := os.WriteFile(filepath.Join(result.PackageDir, "tests", name), []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}

	err = validateStagedPackage(result.PackageDir)
	if err == nil || !strings.Contains(err.Error(), "unknown_group") {
		t.Fatalf("staged package validation did not fail closed: %v", err)
	}
}

func testArtifact(t *testing.T, dir string) problemv1.ArtifactRef {
	t.Helper()
	zipPath, digest, size, err := problemstorage.BuildDeterministicPackageArtifact(filepath.Dir(dir), dir)
	if err != nil {
		t.Fatal(err)
	}
	_ = os.Remove(zipPath)
	return problemv1.ArtifactRef{URI: "file://test", SHA256: digest, SizeBytes: size, ContentType: "application/zip"}
}

func writeTestTree(t *testing.T, root string, files map[string]string) {
	t.Helper()
	if err := os.MkdirAll(root, 0o755); err != nil {
		t.Fatal(err)
	}
	for name, content := range files {
		path := filepath.Join(root, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
}

func snapshotTestTree(t *testing.T, root string) map[string]string {
	t.Helper()
	result := map[string]string{}
	err := filepath.WalkDir(root, func(path string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.IsDir() {
			return nil
		}
		rel, err := filepath.Rel(root, path)
		if err != nil {
			return err
		}
		result[filepath.ToSlash(rel)] = string(mustReadFile(t, path))
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return result
}

func mustReadFile(t *testing.T, path string) []byte {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return data
}
