// Package app assembles contest-service from explicit configuration and platform adapters.
// Process signals and environment loading belong to the executable entry point.
package app

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
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

// Run owns the service component graph until shutdown completes. Bootstrap retains
// responsibility for startup rollback, readiness and reverse-order cleanup.
func Run(ctx context.Context, runtimeConfig config.Config, logger *slog.Logger) error {
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
		bootstrap.KindPermission: contestPermissionFactory(
			runtimeConfig.ServiceContextFile,
			runtimeConfig.Managed,
		),
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
	if err := platform.Start(ctx); err != nil {
		return err
	}
	logger.Info("contest service started", "address", runtimeConfig.ListenAddress, "registration_mode", runtimeConfig.RegistrationMode)
	return platform.Wait(context.Background())
}

func contestPermissionFactory(contextFile string, managed bool) bootstrap.Factory {
	return bootstrap.NewPermissionFactory(bootstrap.PermissionOptions{
		Service:     "contest-service",
		ContextFile: contextFile,
		Managed:     managed,
		BindingName: sharedperm.DefaultPermissionCheckApiID,
	})
}
