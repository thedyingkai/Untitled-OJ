package repository

import (
	"reflect"
	"testing"
)

func TestNormalizeTaskIDsTrimsAndDeduplicatesStreamTaskIDs(t *testing.T) {
	got := normalizeTaskIDs([]string{" sub-42 ", "", "sub-42", "sub-43"})
	want := []string{"sub-42", "sub-43"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("unexpected normalized task ids: got %#v want %#v", got, want)
	}
}
