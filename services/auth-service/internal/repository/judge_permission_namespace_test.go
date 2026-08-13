package repository

import (
	"crypto/sha256"
	"fmt"
	"os"
	"regexp"
	"strings"
	"testing"
)

func TestJudgePermissionNamespaceMigrationIsExpandOnlyAndPublished(t *testing.T) {
	up := readJudgePermissionFixture(t, "../../migrations/000016_judge_permission_namespace.up.sql")
	down := readJudgePermissionFixture(t, "../../migrations/000016_judge_permission_namespace.down.sql")
	oci := readJudgePermissionFixture(t, "../../migrations/migration.oci.yaml")
	release := readJudgePermissionFixture(t, "../../release.yaml")

	for _, permission := range []string{
		"judge.submission.view.own",
		"judge.submission.view.all",
		"judge.submission.manage",
	} {
		if !strings.Contains(up, permission) {
			t.Fatalf("migration does not create %q", permission)
		}
	}
	for _, legacy := range []string{
		"submission.view.own",
		"submission.view.all",
		"submission.rejudge",
		"submission.delete",
	} {
		if !strings.Contains(up, legacy) {
			t.Fatalf("migration does not preserve the legacy mapping for %q", legacy)
		}
	}
	if strings.Contains(strings.ToUpper(down), "DELETE ") ||
		strings.Contains(strings.ToUpper(down), "DROP ") ||
		!strings.Contains(down, "Expand-only rollback by design") {
		t.Fatal("rollback must retain authorization state without destructive SQL")
	}
	if !strings.Contains(up, "permission_code IN ('submission.view.own', 'submission.view.all')") {
		t.Fatal("legacy view.all roles do not receive the current view.own prerequisite")
	}
	if !strings.Contains(up, "delete_submission.permission_code = 'submission.delete'") ||
		!strings.Contains(up, "WHERE rejudge.permission_code = 'submission.rejudge'") {
		t.Fatal("manage permission is not restricted to the legacy rejudge/delete role intersection")
	}
	if !strings.Contains(up, "FROM permission_assignments") ||
		!strings.Contains(up, "AND effect = 'allow'") {
		t.Fatal("direct view authorization is not migrated with deny-safe prerequisite handling")
	}
	if !strings.Contains(up, "permission_code IN ('submission.rejudge', 'submission.delete')") ||
		!strings.Contains(up, "DO UPDATE SET\n    effect = 'deny'") {
		t.Fatal("legacy direct manage denials do not override current allows")
	}

	upName := "000016_judge_permission_namespace.up.sql"
	downName := "000016_judge_permission_namespace.down.sql"
	upIndex := strings.Index(oci, "  - "+upName)
	rollbackIndex := strings.Index(oci, "rollbackFiles:")
	if upIndex < 0 || rollbackIndex < 0 || upIndex >= rollbackIndex ||
		!strings.Contains(oci, "rollbackFiles:\n  - "+downName) {
		t.Fatal("migration OCI manifest does not append up and prepend down in dependency order")
	}
	upBytes, err := os.ReadFile("../../migrations/" + upName)
	if err != nil {
		t.Fatal(err)
	}
	wantChecksum := fmt.Sprintf("sha256:%x", sha256.Sum256(upBytes))
	entry := regexp.MustCompile(`(?s)- version: "000016_judge_permission_namespace"\s+path: services/auth-service/migrations/000016_judge_permission_namespace\.up\.sql\s+checksum: (sha256:[0-9a-f]{64})`).FindStringSubmatch(release)
	if len(entry) != 2 || entry[1] != wantChecksum {
		t.Fatalf("auth release does not publish the exact 000016 bytes: got=%v want=%s", entry, wantChecksum)
	}
}

func readJudgePermissionFixture(t *testing.T, path string) string {
	t.Helper()
	contents, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return string(contents)
}
