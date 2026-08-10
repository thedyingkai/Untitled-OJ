package store

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"

	"ojos-shared/storagecontract"
)

func TestEnsureBucketAllowsObjectWrites(t *testing.T) {
	store, err := NewObjectStore(t.TempDir(), nil)
	if err != nil {
		t.Fatalf("new object store: %v", err)
	}

	created, err := store.EnsureBucket("submissions")
	if err != nil {
		t.Fatalf("ensure bucket: %v", err)
	}
	if !created {
		t.Fatalf("expected new bucket to be created")
	}

	meta, err := store.Put(context.Background(), "submissions", "42/main.cpp", PutOptions{ContentType: "text/plain"}, strings.NewReader("int main(){}"))
	if err != nil {
		t.Fatalf("put object: %v", err)
	}
	if meta.Bucket != "submissions" || meta.Key != "42/main.cpp" || meta.SizeBytes == 0 {
		t.Fatalf("unexpected metadata: %#v", meta)
	}

	buckets := store.BucketNames()
	if len(buckets) != 1 || buckets[0] != "submissions" {
		t.Fatalf("unexpected buckets: %#v", buckets)
	}
}

func TestPutIfAbsentDoesNotReplaceExistingObject(t *testing.T) {
	objectStore, err := NewObjectStore(t.TempDir(), []string{"problems"})
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	first, err := objectStore.Put(ctx, "problems", "package.zip", PutOptions{IfAbsent: true}, strings.NewReader("first"))
	if err != nil {
		t.Fatal(err)
	}
	_, err = objectStore.Put(ctx, "problems", "package.zip", PutOptions{IfAbsent: true}, strings.NewReader("second"))
	if !errors.Is(err, ErrPreconditionFailed) {
		t.Fatalf("expected precondition failure, got %v", err)
	}
	stored, err := objectStore.Metadata("problems", "package.zip")
	if err != nil {
		t.Fatal(err)
	}
	if stored.SHA256 != first.SHA256 || stored.SizeBytes != first.SizeBytes {
		t.Fatalf("existing object changed: got %#v want %#v", stored, first)
	}
}

func TestListObjectsUsesStableCursor(t *testing.T) {
	objectStore, err := NewObjectStore(t.TempDir(), []string{"problems"})
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	for _, key := range []string{"package-sha256-b.zip", "unrelated.txt", "package-sha256-a.zip", "package-sha256-c.zip"} {
		if _, err := objectStore.Put(ctx, "problems", key, PutOptions{}, strings.NewReader(key)); err != nil {
			t.Fatal(err)
		}
	}
	first, err := objectStore.List(ctx, "problems", "package-sha256-", "", 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(first.Objects) != 2 || first.Objects[0].Key != "package-sha256-a.zip" || first.Objects[1].Key != "package-sha256-b.zip" || first.NextCursor != "package-sha256-b.zip" {
		t.Fatalf("unexpected first page: %#v", first)
	}
	second, err := objectStore.List(ctx, "problems", "package-sha256-", first.NextCursor, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(second.Objects) != 1 || second.Objects[0].Key != "package-sha256-c.zip" || second.NextCursor != "" {
		t.Fatalf("unexpected second page: %#v", second)
	}
}

func TestListObjectsRecoversObjectWhoseMetadataWriteWasLost(t *testing.T) {
	objectStore, err := NewObjectStore(t.TempDir(), []string{"problems"})
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	key := "package-sha256-" + strings.Repeat("a", 64) + ".zip"
	written, err := objectStore.Put(ctx, "problems", key, PutOptions{}, strings.NewReader("artifact"))
	if err != nil {
		t.Fatal(err)
	}
	metaPath, err := objectStore.metaPath("problems", key)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(metaPath); err != nil {
		t.Fatal(err)
	}
	page, err := objectStore.List(ctx, "problems", "package-sha256-", "", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(page.Objects) != 1 || page.Objects[0].SHA256 != written.SHA256 || page.Objects[0].SizeBytes != written.SizeBytes {
		t.Fatalf("missing metadata was not recovered: %#v", page)
	}
}

func TestDeleteIfMatchesPreservesObjectOnDigestOrSizeMismatch(t *testing.T) {
	objectStore, err := NewObjectStore(t.TempDir(), []string{"problems"})
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	meta, err := objectStore.Put(ctx, "problems", "immutable.zip", PutOptions{}, strings.NewReader("immutable"))
	if err != nil {
		t.Fatal(err)
	}
	if err := objectStore.DeleteIfMatches(ctx, "problems", "immutable.zip", strings.Repeat("0", 64), meta.SizeBytes); !errors.Is(err, ErrPreconditionFailed) {
		t.Fatalf("digest mismatch must fail closed, got %v", err)
	}
	if err := objectStore.DeleteIfMatches(ctx, "problems", "immutable.zip", meta.SHA256, meta.SizeBytes+1); !errors.Is(err, ErrPreconditionFailed) {
		t.Fatalf("size mismatch must fail closed, got %v", err)
	}
	if _, err := objectStore.Metadata("problems", "immutable.zip"); err != nil {
		t.Fatalf("mismatch deleted object: %v", err)
	}
	if err := objectStore.DeleteIfMatches(ctx, "problems", "immutable.zip", meta.SHA256, meta.SizeBytes); err != nil {
		t.Fatal(err)
	}
	if _, err := objectStore.Metadata("problems", "immutable.zip"); !os.IsNotExist(err) {
		t.Fatalf("matching conditional delete did not remove object: %v", err)
	}
}

func TestDeleteIfMatchesAllowsEmptyObject(t *testing.T) {
	objectStore, err := NewObjectStore(t.TempDir(), []string{"problems"})
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	meta, err := objectStore.Put(ctx, "problems", "empty.in", PutOptions{}, strings.NewReader(""))
	if err != nil {
		t.Fatal(err)
	}
	if meta.SizeBytes != 0 {
		t.Fatalf("empty object size = %d, want 0", meta.SizeBytes)
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodHead, "/objects/problems/empty.in", nil)
	if err != nil {
		t.Fatal(err)
	}
	rec := httptest.NewRecorder()
	if err := objectStore.Serve(rec, req, "problems", "empty.in"); err != nil {
		t.Fatal(err)
	}
	if got := rec.Header().Get("Content-Length"); got != "0" {
		t.Fatalf("empty object HEAD Content-Length = %q, want 0", got)
	}
	if got := rec.Header().Get(storagecontract.ResultHeader); got != storagecontract.ResultPresent {
		t.Fatalf("empty object HEAD result = %q, want %q", got, storagecontract.ResultPresent)
	}

	if err := objectStore.DeleteIfMatches(ctx, "problems", "empty.in", meta.SHA256, 0); err != nil {
		t.Fatalf("conditional delete empty object: %v", err)
	}
	if _, err := objectStore.Metadata("problems", "empty.in"); !os.IsNotExist(err) {
		t.Fatalf("empty object still exists after conditional delete: %v", err)
	}
	missingReq := httptest.NewRequest(http.MethodHead, "/objects/problems/empty.in", nil)
	missingRec := httptest.NewRecorder()
	if err := objectStore.Serve(missingRec, missingReq, "problems", "empty.in"); !errors.Is(err, ErrObjectNotFound) {
		t.Fatalf("missing local HEAD error = %v, want ErrObjectNotFound", err)
	}
	if got := missingRec.Header().Get(storagecontract.ResultHeader); got != storagecontract.ResultObjectNotFound {
		t.Fatalf("missing local HEAD result = %q, want %q", got, storagecontract.ResultObjectNotFound)
	}
}
