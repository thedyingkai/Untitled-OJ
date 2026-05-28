package logger

import (
	"context"

	"go.opentelemetry.io/otel/trace"
	"go.uber.org/zap"
)

func New(service string) (*zap.Logger, error) {
	return zap.NewProduction(
		zap.Fields(
			zap.String("service", service),
		),
	)
}

func WithTrace(ctx context.Context, log *zap.Logger) *zap.Logger {
	spanCtx := trace.SpanContextFromContext(ctx)

	if !spanCtx.IsValid() {
		return log
	}

	return log.With(
		zap.String("trace_id", spanCtx.TraceID().String()),
		zap.String("span_id", spanCtx.SpanID().String()),
	)
}
