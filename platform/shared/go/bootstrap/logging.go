package bootstrap

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"os"
)

const ValueLogger = "platform.logger"

type LoggingOptions struct {
	Logger    *slog.Logger
	Writer    io.Writer
	Level     slog.Leveler
	ValueName string
}

func NewLoggingFactory(options LoggingOptions) Factory {
	return FactoryFunc(func(BuildContext) (Component, error) {
		logger := options.Logger
		if logger == nil {
			writer := options.Writer
			if writer == nil {
				writer = os.Stdout
			}
			logger = slog.New(slog.NewJSONHandler(writer, &slog.HandlerOptions{Level: options.Level}))
		}
		name := defaultValueName(options.ValueName, ValueLogger)
		if !validToken(name) {
			return nil, errors.New("logging output name is invalid")
		}
		return &ComponentFuncs{Values: map[string]any{name: logger}}, nil
	})
}

// TracingProvider is intentionally SDK-neutral. The bootstrap runtime owns its
// shutdown without requiring a process-global tracer singleton.
type TracingProvider interface {
	Shutdown(context.Context) error
}

type TracingInit func(context.Context, string) (TracingProvider, error)

type TracingOptions struct {
	Service   string
	Init      TracingInit
	ValueName string
}

const ValueTracingProvider = "platform.tracing"

func NewTracingFactory(options TracingOptions) Factory {
	return FactoryFunc(func(build BuildContext) (Component, error) {
		if options.Init == nil {
			return nil, errors.New("tracing initializer is required")
		}
		name := defaultValueName(options.ValueName, ValueTracingProvider)
		if !validToken(name) {
			return nil, errors.New("tracing output name is invalid")
		}
		service := options.Service
		if service == "" {
			service = build.Spec.Name
		}
		if !validToken(service) {
			return nil, errors.New("tracing service identity is invalid")
		}
		component := &tracingComponent{service: service, init: options.Init, valueName: name}
		return component, nil
	})
}

type tracingComponent struct {
	service   string
	init      TracingInit
	valueName string
	provider  TracingProvider
}

func (component *tracingComponent) Start(ctx context.Context) error {
	provider, err := component.init(ctx, component.service)
	if err != nil {
		return errors.New("initialize tracing provider")
	}
	if provider == nil {
		return errors.New("initialize tracing provider")
	}
	component.provider = provider
	return nil
}

func (component *tracingComponent) Close(ctx context.Context) error {
	if component.provider == nil {
		return nil
	}
	if err := component.provider.Shutdown(ctx); err != nil {
		return errors.New("shutdown tracing provider")
	}
	return nil
}

func (component *tracingComponent) Outputs() map[string]any {
	if component.provider == nil {
		return nil
	}
	return map[string]any{component.valueName: component.provider}
}

func defaultValueName(value, fallback string) string {
	if value == "" {
		return fallback
	}
	return value
}
