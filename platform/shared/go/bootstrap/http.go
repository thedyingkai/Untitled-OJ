package bootstrap

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"sync"
	"time"
)

const (
	ValueHTTPServer  = "platform.http-server"
	ValueHTTPAddress = "platform.http-address"
)

type HTTPHandlerFactory func(Resolver, Prober) (http.Handler, error)

type HTTPOptions struct {
	Address           string
	Handler           HTTPHandlerFactory
	ReadHeaderTimeout time.Duration
	ReadTimeout       time.Duration
	WriteTimeout      time.Duration
	IdleTimeout       time.Duration
	ServerValue       string
	AddressValue      string
}

func NewHTTPFactory(options HTTPOptions) Factory {
	return FactoryFunc(func(build BuildContext) (Component, error) {
		if options.Address == "" || options.Handler == nil {
			return nil, errors.New("HTTP listener options are incomplete")
		}
		handler, err := options.Handler(build.Values, build.Probes)
		if err != nil || handler == nil {
			return nil, errors.New("build HTTP handler")
		}
		serverName := defaultValueName(options.ServerValue, ValueHTTPServer)
		addressName := defaultValueName(options.AddressValue, ValueHTTPAddress)
		if !validToken(serverName) || !validToken(addressName) || serverName == addressName {
			return nil, errors.New("HTTP output names are invalid")
		}
		server := &http.Server{
			Addr: options.Address, Handler: handler,
			ReadHeaderTimeout: options.ReadHeaderTimeout, ReadTimeout: options.ReadTimeout,
			WriteTimeout: options.WriteTimeout, IdleTimeout: options.IdleTimeout,
		}
		return &httpComponent{server: server, serverValue: serverName, addressValue: addressName, failures: make(chan error, 1)}, nil
	})
}

type httpComponent struct {
	server       *http.Server
	serverValue  string
	addressValue string
	failures     chan error

	mu       sync.RWMutex
	listener net.Listener
}

func (component *httpComponent) Start(context.Context) error {
	listener, err := net.Listen("tcp", component.server.Addr)
	if err != nil {
		return errors.New("listen for HTTP requests")
	}
	component.mu.Lock()
	component.listener = listener
	component.mu.Unlock()
	go func() {
		if err := component.server.Serve(listener); err != nil && !errors.Is(err, http.ErrServerClosed) {
			component.failures <- errors.New("HTTP server stopped unexpectedly")
		}
		close(component.failures)
	}()
	return nil
}

func (component *httpComponent) Close(ctx context.Context) error {
	if err := component.server.Shutdown(ctx); err != nil {
		_ = component.server.Close()
		return errors.New("shutdown HTTP server")
	}
	return nil
}

func (component *httpComponent) Health(context.Context) error {
	component.mu.RLock()
	defer component.mu.RUnlock()
	if component.listener == nil {
		return errors.New("HTTP listener is not started")
	}
	return nil
}

func (component *httpComponent) Errors() <-chan error { return component.failures }

func (component *httpComponent) Outputs() map[string]any {
	component.mu.RLock()
	defer component.mu.RUnlock()
	if component.listener == nil {
		return nil
	}
	return map[string]any{
		component.serverValue:  component.server,
		component.addressValue: component.listener.Addr().String(),
	}
}

type PublicProbeFailure struct {
	Code    string
	Message string
}

type ProbeHTTPOptions struct {
	HealthPath string
	ReadyPath  string
	Failures   map[string]PublicProbeFailure
}

// WithProbeEndpoints mounts platform-owned liveness/readiness responses in
// front of a service handler. Responses never contain dependency errors.
func WithProbeEndpoints(next http.Handler, prober Prober, options ProbeHTTPOptions) http.Handler {
	if next == nil {
		next = http.NotFoundHandler()
	}
	healthPath := options.HealthPath
	if healthPath == "" {
		healthPath = "/healthz"
	}
	readyPath := options.ReadyPath
	if readyPath == "" {
		readyPath = "/readyz"
	}
	return http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.Method != http.MethodGet || (request.URL.Path != healthPath && request.URL.Path != readyPath) {
			next.ServeHTTP(writer, request)
			return
		}
		if prober == nil {
			writer.Header().Set("Content-Type", "application/json; charset=utf-8")
			writer.WriteHeader(http.StatusServiceUnavailable)
			_ = json.NewEncoder(writer).Encode(map[string]string{"code": "service_unavailable", "message": "service is unavailable"})
			return
		}
		report := prober.Health(request.Context())
		if request.URL.Path == readyPath {
			report = prober.Ready(request.Context())
		}
		writer.Header().Set("Content-Type", "application/json; charset=utf-8")
		if report.OK() {
			writer.WriteHeader(http.StatusOK)
			_ = json.NewEncoder(writer).Encode(map[string]string{"status": "ok"})
			return
		}
		failure := PublicProbeFailure{Code: "service_unavailable", Message: "service is unavailable"}
		for _, result := range report.Components {
			if result.Status != ProbeOK {
				if configured, exists := options.Failures[result.Name]; exists {
					failure = configured
				}
				break
			}
		}
		writer.WriteHeader(http.StatusServiceUnavailable)
		_ = json.NewEncoder(writer).Encode(map[string]string{"code": failure.Code, "message": failure.Message})
	})
}
