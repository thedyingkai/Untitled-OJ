package tracing

import (
	"context"
	"strings"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracegrpc"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/sdk/resource"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	semconv "go.opentelemetry.io/otel/semconv/v1.37.0"
)

const (
	// Keep tracing best-effort. Request completion must never wait for the
	// collector, and a disconnected collector must not allow memory usage to
	// grow without bound.
	maxQueuedSpans     = 2048
	maxExportBatchSize = 512
	batchTimeout       = time.Second
	exportTimeout      = 3 * time.Second
)

func InitOTLP(ctx context.Context, serviceName string, endpoint string) (*sdktrace.TracerProvider, error) {
	res, err := resource.New(
		ctx,
		resource.WithAttributes(
			semconv.ServiceName(serviceName),
		),
	)
	if err != nil {
		return nil, err
	}

	opts := []sdktrace.TracerProviderOption{
		sdktrace.WithResource(res),
		sdktrace.WithSampler(sdktrace.AlwaysSample()),
	}

	if strings.TrimSpace(endpoint) != "" {
		exporter, err := otlptracegrpc.New(
			ctx,
			otlptracegrpc.WithEndpoint(endpoint),
			otlptracegrpc.WithInsecure(),
		)
		if err != nil {
			return nil, err
		}
		opts = append(opts, sdktrace.WithSpanProcessor(newBatchSpanProcessor(exporter)))
	}

	tp := sdktrace.NewTracerProvider(opts...)

	otel.SetTracerProvider(tp)
	otel.SetTextMapPropagator(
		propagation.NewCompositeTextMapPropagator(
			propagation.TraceContext{},
			propagation.Baggage{},
		),
	)

	return tp, nil
}

func newBatchSpanProcessor(exporter sdktrace.SpanExporter) sdktrace.SpanProcessor {
	return sdktrace.NewBatchSpanProcessor(
		exporter,
		sdktrace.WithMaxQueueSize(maxQueuedSpans),
		sdktrace.WithMaxExportBatchSize(maxExportBatchSize),
		sdktrace.WithBatchTimeout(batchTimeout),
		sdktrace.WithExportTimeout(exportTimeout),
	)
}
