package tracing

import (
	"context"
	"sync"
	"testing"
	"time"

	sdktrace "go.opentelemetry.io/otel/sdk/trace"
)

type blockingSpanExporter struct {
	started chan struct{}
	release chan struct{}
	once    sync.Once
}

func (e *blockingSpanExporter) ExportSpans(ctx context.Context, _ []sdktrace.ReadOnlySpan) error {
	e.once.Do(func() { close(e.started) })
	select {
	case <-e.release:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (*blockingSpanExporter) Shutdown(context.Context) error { return nil }

func TestBatchSpanProcessorDoesNotBlockWhenExporterIsUnavailable(t *testing.T) {
	exporter := &blockingSpanExporter{
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	tp := sdktrace.NewTracerProvider(
		sdktrace.WithSpanProcessor(newBatchSpanProcessor(exporter)),
	)
	tracer := tp.Tracer("tracing-test")

	// Fill the first export batch so the exporter is known to be blocked.
	for range maxExportBatchSize {
		_, span := tracer.Start(context.Background(), "fill-export-batch")
		span.End()
	}
	select {
	case <-exporter.started:
	case <-time.After(2 * time.Second):
		t.Fatal("batch exporter did not start")
	}

	// Saturate the bounded queue while the exporter remains unavailable. Span
	// completion is request-path work and must drop excess telemetry instead of
	// waiting for the collector.
	done := make(chan struct{})
	go func() {
		defer close(done)
		for range maxQueuedSpans * 2 {
			_, span := tracer.Start(context.Background(), "request")
			span.End()
		}
	}()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("span completion blocked on an unavailable exporter")
	}

	close(exporter.release)
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if err := tp.Shutdown(shutdownCtx); err != nil {
		t.Fatalf("shutdown tracer provider: %v", err)
	}
}
