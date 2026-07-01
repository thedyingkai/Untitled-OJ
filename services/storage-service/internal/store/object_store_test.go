package store

import (
	"strings"
	"testing"
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

	meta, err := store.Put("submissions", "42/main.cpp", "text/plain", strings.NewReader("int main(){}"))
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
