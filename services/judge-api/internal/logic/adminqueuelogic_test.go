package logic

import (
	"os"
	"strings"
	"testing"
)

func TestAdminQueueReportsRedisStreamLagAndPendingRange(t *testing.T) {
	data, err := os.ReadFile("adminqueuelogic.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)
	for _, want := range []string{
		"XInfoGroups",
		"ConsumerLag",
		"ConsumerCount",
		"PendingLowestId",
		"PendingHighestId",
		"RedisStatus",
		"XPendingExt",
	} {
		if !strings.Contains(source, want) {
			t.Fatalf("AdminQueue must expose Redis Stream lag/pending observability; missing %q", want)
		}
	}
}
