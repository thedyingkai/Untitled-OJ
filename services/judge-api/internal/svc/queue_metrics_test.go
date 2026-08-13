package svc

import (
	"strings"
	"testing"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

func TestJudgeQueueCollectorFailsVisibleWithoutDatabase(t *testing.T) {
	registry := prometheus.NewPedanticRegistry()
	registry.MustRegister(NewJudgeQueueCollector(&ServiceContext{}))
	want := `
# HELP ojos_judge_queue_metrics_collection_error Whether the authoritative Judge PostgreSQL metrics query failed.
# TYPE ojos_judge_queue_metrics_collection_error gauge
ojos_judge_queue_metrics_collection_error 1
`
	if err := testutil.GatherAndCompare(
		registry,
		strings.NewReader(want),
		"ojos_judge_queue_metrics_collection_error",
	); err != nil {
		t.Fatal(err)
	}
}
