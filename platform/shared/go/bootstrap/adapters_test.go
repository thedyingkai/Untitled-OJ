package bootstrap

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"reflect"
	"sync"
	"testing"
	"time"
)

func TestHTTPFactoryOwnsListenerProbesAndGracefulShutdown(t *testing.T) {
	manifest := Manifest{
		Service: "http-test", ShutdownTimeout: time.Second, ProbeTimeout: 100 * time.Millisecond,
		Components: []ComponentSpec{
			{Name: "dependency", Kind: KindDomain},
			{Name: "http", Kind: KindHTTP, DependsOn: []string{"dependency"}},
		},
	}
	runtime, err := New(manifest, Options{Factories: map[Kind]Factory{
		KindDomain: FactoryFunc(func(BuildContext) (Component, error) {
			return &ComponentFuncs{ReadyFunc: func(context.Context) error { return io.EOF }}, nil
		}),
		KindHTTP: NewHTTPFactory(HTTPOptions{
			Address: "127.0.0.1:0", ReadHeaderTimeout: time.Second,
			Handler: func(_ Resolver, prober Prober) (http.Handler, error) {
				return WithProbeEndpoints(http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) {
					writer.WriteHeader(http.StatusNoContent)
				}), prober, ProbeHTTPOptions{Failures: map[string]PublicProbeFailure{
					"dependency": {Code: "dependency_unavailable", Message: "dependency is unavailable"},
				}}), nil
			},
		}),
	}})
	if err != nil {
		t.Fatal(err)
	}
	if err := runtime.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	address, err := Resolve[string](runtime, ValueHTTPAddress)
	if err != nil {
		t.Fatal(err)
	}
	client := &http.Client{Timeout: time.Second}
	health, err := client.Get("http://" + address + "/healthz")
	if err != nil {
		t.Fatal(err)
	}
	defer health.Body.Close()
	if health.StatusCode != http.StatusOK {
		t.Fatalf("health status = %d", health.StatusCode)
	}
	ready, err := client.Get("http://" + address + "/readyz")
	if err != nil {
		t.Fatal(err)
	}
	defer ready.Body.Close()
	var body map[string]string
	if err := json.NewDecoder(ready.Body).Decode(&body); err != nil {
		t.Fatal(err)
	}
	if ready.StatusCode != http.StatusServiceUnavailable || body["code"] != "dependency_unavailable" {
		t.Fatalf("ready status=%d body=%#v", ready.StatusCode, body)
	}
	if err := runtime.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	if _, err := client.Get("http://" + address + "/healthz"); err == nil {
		t.Fatal("listener remained reachable after graceful shutdown")
	}
}

type recordingTracingProvider struct {
	lock   *sync.Mutex
	events *[]string
}

func (provider recordingTracingProvider) Shutdown(context.Context) error {
	provider.lock.Lock()
	defer provider.lock.Unlock()
	*provider.events = append(*provider.events, "close:tracing")
	return nil
}

func TestLoggingAndTracingAreInstanceScopedAndCloseInReverseOrder(t *testing.T) {
	var lock sync.Mutex
	var events []string
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	manifest := Manifest{Service: "telemetry-test", Components: []ComponentSpec{
		{Name: "logging", Kind: KindLogging},
		{Name: "tracing", Kind: KindTracing, DependsOn: []string{"logging"}},
	}}
	runtime, err := New(manifest, Options{Factories: map[Kind]Factory{
		KindLogging: NewLoggingFactory(LoggingOptions{Logger: logger}),
		KindTracing: NewTracingFactory(TracingOptions{
			Service: "telemetry-test",
			Init: func(_ context.Context, service string) (TracingProvider, error) {
				lock.Lock()
				events = append(events, "start:"+service)
				lock.Unlock()
				return recordingTracingProvider{lock: &lock, events: &events}, nil
			},
		}),
	}})
	if err != nil {
		t.Fatal(err)
	}
	if err := runtime.Start(context.Background()); err != nil {
		t.Fatal(err)
	}
	resolved, err := Resolve[*slog.Logger](runtime, ValueLogger)
	if err != nil || resolved != logger {
		t.Fatalf("logger=%p want=%p err=%v", resolved, logger, err)
	}
	if _, err := Resolve[TracingProvider](runtime, ValueTracingProvider); err != nil {
		t.Fatal(err)
	}
	if err := runtime.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	lock.Lock()
	defer lock.Unlock()
	if want := []string{"start:telemetry-test", "close:tracing"}; !reflect.DeepEqual(events, want) {
		t.Fatalf("events=%#v want=%#v", events, want)
	}
}
