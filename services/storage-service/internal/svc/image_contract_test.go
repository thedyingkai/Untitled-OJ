package svc

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestRuntimeImageSeedsFreshNamedVolumeForNonRootUser(t *testing.T) {
	bytes, err := os.ReadFile(filepath.Join("..", "..", "Dockerfile"))
	if err != nil {
		t.Fatal(err)
	}
	dockerfile := string(bytes)
	seed := "RUN install -d -m 0750 /out/storage-root /out/storage-root/objects /out/storage-root/metadata"
	copySeed := "COPY --from=build --chown=65532:65532 --chmod=0750 /out/storage-root /data/ojos/storage"
	nonRoot := "USER 65532:65532"

	seedIndex := strings.Index(dockerfile, seed)
	copyIndex := strings.Index(dockerfile, copySeed)
	userIndex := strings.Index(dockerfile, nonRoot)
	for value, index := range map[string]int{
		seed:     seedIndex,
		copySeed: copyIndex,
		nonRoot:  userIndex,
	} {
		if index < 0 {
			t.Fatalf("runtime image does not preserve fresh-volume ownership: missing %q", value)
		}
	}
	if seedIndex > copyIndex {
		t.Fatal("runtime image copies the local-store seed before creating it")
	}
	if copyIndex > userIndex {
		t.Fatal("runtime image switches to nonroot before seeding the writable volume target")
	}
}
