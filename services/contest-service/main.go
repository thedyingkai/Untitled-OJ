package main

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/prometheus/client_golang/prometheus"
	"ojos-contest-service/internal/config"
	"ojos-contest-service/internal/contest"
	"ojos-contest-service/internal/httpapi"
	"ojos-contest-service/internal/problemclient"
	"ojos-shared/bootstrap"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/servicecontext"
)

const contestHandlerValue = "contest.http-handler"

func main() {
	if len(os.Args) == 2 && (os.Args[1] == "healthcheck" || os.Args[1] == "readycheck") {
		client := &http.Client{Timeout: 2 * time.Second}
		path := "/healthz"
		if os.Args[1] == "readycheck" {
			path = "/readyz"
		}
		response, err := client.Get("http://127.0.0.1:8080" + path)
		if err != nil || response.StatusCode != http.StatusOK {
			os.Exit(1)
		}
		_ = response.Body.Close()
		return
	}
	logger := slog.New(slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo}))
	if err := run(logger); err != nil {
		logger.Error("contest service stopped", "error", err)
		os.Exit(1)
	}
}

func run(logger *slog.Logger) error {
	runtimeConfig, err := config.Load()
	if err != nil {
		return err
	}
	rootContext, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	manifest := bootstrap.Manifest{
		Service: "contest-service", ShutdownTimeout: 15 * time.Second, ProbeTimeout: time.Second,
		Components: []bootstrap.ComponentSpec{
			{Name: "logging", Kind: bootstrap.KindLogging},
			{Name: "database", Kind: bootstrap.KindPostgreSQL, DependsOn: []string{"logging"}},
			{Name: "permissions", Kind: bootstrap.KindPermission, DependsOn: []string{"logging"}},
			{Name: "events", Kind: bootstrap.KindEventRelay, DependsOn: []string{"database"}},
			{Name: "domain", Kind: bootstrap.KindDomain, DependsOn: []string{"database", "permissions", "events"}},
			{Name: "http", Kind: bootstrap.KindHTTP, DependsOn: []string{"domain"}},
		},
	}
	factories := map[bootstrap.Kind]bootstrap.Factory{
		bootstrap.KindLogging: bootstrap.NewLoggingFactory(bootstrap.LoggingOptions{Logger: logger}),
		bootstrap.KindPostgreSQL: bootstrap.NewPostgreSQLFactory(bootstrap.PostgreSQLOptions{
			ResourceOutputFile: runtimeConfig.DatabaseSecretFile,
			ConnectTimeout:     5 * time.Second,
		}),
		bootstrap.KindPermission: bootstrap.NewPermissionFactory(bootstrap.PermissionOptions{
			Service: "contest-service", ContextFile: runtimeConfig.ServiceContextFile,
			Managed: runtimeConfig.Managed, BindingName: sharedperm.DefaultPermissionCheckBinding,
		}),
		bootstrap.KindEventRelay: bootstrap.NewEventRelayFactory(bootstrap.EventRelayOptions{
			Service: "contest-service", PublishTypes: []string{contest.ContestCreatedEventType},
			RelayID: "contest-service",
		}),
		bootstrap.KindDomain: bootstrap.FactoryFunc(func(build bootstrap.BuildContext) (bootstrap.Component, error) {
			pool, resolveErr := bootstrap.Resolve[*pgxpool.Pool](build.Values, bootstrap.ValuePostgreSQL)
			if resolveErr != nil {
				return nil, errors.New("contest database is unavailable")
			}
			repository, repositoryErr := contest.NewPostgresRepository(pool)
			if repositoryErr != nil {
				return nil, errors.New("build contest repository")
			}
			var problemReader httpapi.ProblemReader
			if value, exists := build.Values.Lookup(bootstrap.ValueServiceContext); exists {
				provider, valid := value.(*servicecontext.ContextProvider)
				if !valid {
					return nil, errors.New("managed service context is invalid")
				}
				problemReader = problemclient.New(provider)
			}
			var permissionChecker sharedperm.UserChecker
			if value, exists := build.Values.Lookup(bootstrap.ValuePermissionChecker); exists {
				permissionChecker, _ = value.(sharedperm.UserChecker)
			}
			if runtimeConfig.Managed && (problemReader == nil || permissionChecker == nil) {
				return nil, errors.New("managed service dependencies are unavailable")
			}
			handler, handlerErr := httpapi.New(repository, problemReader, permissionChecker, logger, prometheus.NewRegistry())
			if handlerErr != nil {
				return nil, errors.New("build contest HTTP handler")
			}
			return &bootstrap.ComponentFuncs{
				ReadyFunc: func(ctx context.Context) error {
					if problemReader == nil {
						return nil
					}
					return problemReader.Probe(ctx)
				},
				Values: map[string]any{contestHandlerValue: handler.Routes()},
			}, nil
		}),
		bootstrap.KindHTTP: bootstrap.NewHTTPFactory(bootstrap.HTTPOptions{
			Address: runtimeConfig.ListenAddress,
			Handler: func(values bootstrap.Resolver, prober bootstrap.Prober) (http.Handler, error) {
				handler, resolveErr := bootstrap.Resolve[http.Handler](values, contestHandlerValue)
				if resolveErr != nil {
					return nil, errors.New("contest HTTP handler is unavailable")
				}
				return bootstrap.WithProbeEndpoints(handler, prober, bootstrap.ProbeHTTPOptions{
					Failures: map[string]bootstrap.PublicProbeFailure{
						"database": {Code: "database_unavailable", Message: "database is unavailable"},
						"domain":   {Code: "problem_api_unavailable", Message: "required Problem API is unavailable"},
					},
				}), nil
			},
			ReadHeaderTimeout: 5 * time.Second, ReadTimeout: 15 * time.Second,
			WriteTimeout: 30 * time.Second, IdleTimeout: 60 * time.Second,
		}),
	}
	platform, err := bootstrap.New(manifest, bootstrap.Options{Factories: factories})
	if err != nil {
		return err
	}
	if err := platform.Start(rootContext); err != nil {
		return err
	}
	logger.Info("contest service started", "address", runtimeConfig.ListenAddress, "registration_mode", runtimeConfig.RegistrationMode)
	return platform.Wait(context.Background())
}
