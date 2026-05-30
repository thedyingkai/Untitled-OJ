package tracing

import (
	"context"
	"ojos-shared/configs"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracegrpc"
	"go.opentelemetry.io/otel/sdk/resource"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	semconv "go.opentelemetry.io/otel/semconv/v1.41.0"
)

func Init(ctx context.Context, cfg *configs.Config) (*sdktrace.TracerProvider, error) {
	//fmt.Println("otel endpoint:", cfg.Jaeger.Endpoint)

	client := otlptracegrpc.NewClient(
		otlptracegrpc.WithEndpoint(cfg.Jaeger.Endpoint),
		otlptracegrpc.WithInsecure(),
	)

	otlpExporter, err := otlptrace.New(ctx, client)
	if err != nil {
		return nil, err
	}

	//stdoutExporter, err := stdouttrace.New(
	//	stdouttrace.WithPrettyPrint(),
	//)
	//if err != nil {
	//	return nil, err
	//}

	res, err := resource.Merge(
		resource.Default(),
		resource.NewWithAttributes(
			semconv.SchemaURL,
			semconv.ServiceName(cfg.Service.Name),
		),
	)
	if err != nil {
		return nil, err
	}

	tp := sdktrace.NewTracerProvider(
		sdktrace.WithResource(res),

		//sdktrace.WithSpanProcessor(
		//	sdktrace.NewSimpleSpanProcessor(otlpExporter),
		//),

		sdktrace.WithBatcher(otlpExporter),

		//sdktrace.WithSpanProcessor(
		//	sdktrace.NewSimpleSpanProcessor(stdoutExporter),
		//),
	)

	otel.SetTracerProvider(tp)

	return tp, nil
}
