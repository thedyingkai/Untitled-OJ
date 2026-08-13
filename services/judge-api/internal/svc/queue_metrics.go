package svc

import (
	"context"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"go.uber.org/zap"
)

const judgeMetricsQueryTimeout = 750 * time.Millisecond

// JudgeQueueCollector reads the two operational gauges from the same local
// PostgreSQL claim that is authoritative for Worker/task state. It never
// needs a control-plane copy of the generated DSN.
type JudgeQueueCollector struct {
	service *ServiceContext
}

func NewJudgeQueueCollector(service *ServiceContext) *JudgeQueueCollector {
	return &JudgeQueueCollector{service: service}
}

var (
	judgeWorkersOnline = prometheus.NewDesc(
		"ojos_judge_workers_online",
		"Judge workers with a recent heartbeat.",
		nil, nil,
	)
	judgeQueuePending = prometheus.NewDesc(
		"ojos_judge_queue_pending_tasks",
		"Pending judge tasks in the authoritative PostgreSQL queue.",
		nil, nil,
	)
	judgeQueueCollectionError = prometheus.NewDesc(
		"ojos_judge_queue_metrics_collection_error",
		"Whether the authoritative Judge PostgreSQL metrics query failed.",
		nil, nil,
	)
)

func (collector *JudgeQueueCollector) Describe(output chan<- *prometheus.Desc) {
	output <- judgeWorkersOnline
	output <- judgeQueuePending
	output <- judgeQueueCollectionError
}

func (collector *JudgeQueueCollector) Collect(output chan<- prometheus.Metric) {
	if collector == nil || collector.service == nil || collector.service.DB == nil {
		output <- prometheus.MustNewConstMetric(judgeQueueCollectionError, prometheus.GaugeValue, 1)
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), judgeMetricsQueryTimeout)
	defer cancel()
	var online, pending int64
	err := collector.service.DB.QueryRow(ctx, `
		SELECT
			(SELECT COUNT(*) FROM judge_workers
			 WHERE status = 'ONLINE' AND drain = FALSE
			   AND last_seen >= NOW() - INTERVAL '120 seconds'),
			(SELECT COUNT(*) FROM judge_tasks WHERE status = 'PENDING')
	`).Scan(&online, &pending)
	if err != nil {
		output <- prometheus.MustNewConstMetric(judgeQueueCollectionError, prometheus.GaugeValue, 1)
		if collector.service.Logger != nil {
			collector.service.Logger.Warn("collect authoritative Judge queue metrics", zap.Error(err))
		}
		return
	}
	output <- prometheus.MustNewConstMetric(judgeWorkersOnline, prometheus.GaugeValue, float64(online))
	output <- prometheus.MustNewConstMetric(judgeQueuePending, prometheus.GaugeValue, float64(pending))
	output <- prometheus.MustNewConstMetric(judgeQueueCollectionError, prometheus.GaugeValue, 0)
}
