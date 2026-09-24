// Resource construction and startup rollback. No process termination here.
package svc

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"ojos-gateway/internal/authclient"
	"ojos-gateway/internal/config"
	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/proxy"
	gtopology "ojos-gateway/internal/topologyprojection"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/security/internalauth"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/servicecontext"
	"ojos-shared/tracing"

	"github.com/redis/go-redis/v9"
	"go.uber.org/zap"
)

func NewServiceContext(c config.Config) (result *ServiceContext, startupErr error) {
	serviceContext := &ServiceContext{}
	defer func() {
		if result == nil {
			shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			serviceContext.Close(shutdownCtx)
		}
	}()
	ctx := context.Background()
	if err := applyEnvOverrides(&c); err != nil {
		return nil, fmt.Errorf("configure Gateway: %w", err)
	}
	if err := validateWorkloadIdentityConfig(
		c.WorkloadIdentity,
		productionModeEnabled(),
	); err != nil {
		return nil, fmt.Errorf("configure workload identity: %w", err)
	}

	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		return nil, fmt.Errorf("init logger failed: %w", err)
	}
	serviceContext.Logger = zlog

	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		return nil, fmt.Errorf("init tracing failed: %w", err)
	}
	serviceContext.Tracer = tp

	redisOptions, err := redis.ParseURL(c.Redis.Url)
	if err != nil {
		return nil, fmt.Errorf("parse redis url failed: %w", err)
	}
	redisClient := redis.NewClient(redisOptions)
	serviceContext.Redis = redisClient
	if err := redisClient.Ping(ctx).Err(); err != nil {
		return nil, fmt.Errorf("ping redis failed: %w", err)
	}

	var internalSigner *internalauth.Signer
	if c.InternalAuth.Enabled {
		zlog.Warn("gateway internal request signing is disabled until internal auth keys are provided by a service-owned API")
	}

	authClient := authclient.New(c.AuthService.Endpoint)
	var contextProvider *servicecontext.ContextProvider
	var permissionChecker sharedperm.UserChecker
	contextValue, err := servicecontext.LoadOptional()
	if err != nil {
		return nil, fmt.Errorf("load managed Service Context failed: %w", err)
	}
	if contextValue != nil {
		if err := contextValue.RequireService("gateway"); err != nil {
			return nil, fmt.Errorf("validate managed Service Context failed: %w", err)
		}
		contextPath := strings.TrimSpace(os.Getenv("OJOS_SERVICE_CONTEXT_FILE"))
		if contextPath == "" {
			contextPath = servicecontext.DefaultFile
		}
		contextProvider, err = servicecontext.NewContextProvider(contextPath, servicecontext.ProviderOptions{})
		serviceContext.Context = contextProvider
		if err == nil {
			permissionChecker, err = sharedperm.NewContextProviderUserChecker(contextProvider, permissionBindingName)
		}
		if err == nil {
			err = contextProvider.Start(context.Background())
		}
		if err != nil {
			return nil, fmt.Errorf("configure permission ApiBinding failed: %w", err)
		}
	} else if managedEnvironment() {
		return nil, errors.New("managed Gateway requires an Agent Service Context")
	}
	serviceProxy, err := proxy.NewServiceProxy(c.Proxy.Routes, c.Proxy.TrustedServices, c.Jwt.Secret, internalSigner, zlog)
	if err != nil {
		return nil, fmt.Errorf("init proxy failed: %w", err)
	}
	serviceContext.ServiceProxy = serviceProxy
	serviceProxy.SetNodeID(c.Orchestrator.NodeID)
	if strings.TrimSpace(c.WorkloadIdentity.PublicKeyFile) != "" {
		workloadVerifier, verifyErr := workloadIdentityVerifier(c.WorkloadIdentity)
		if verifyErr != nil {
			return nil, fmt.Errorf("configure workload identity verifier failed: %w", verifyErr)
		}
		serviceProxy.SetWorkloadVerifier(workloadVerifier)
	}
	serviceProxy.SetPermissionChecker(func(ctx context.Context, authHeader string, caller proxy.PermissionCheckCaller, permissionCode string) (bool, error) {
		if permissionChecker != nil {
			return permissionChecker.HasUserPermission(ctx, caller.UserID, permissionCode, sharedperm.Scope{
				Type: caller.ScopeType,
				ID:   caller.ScopeID,
			})
		}
		return authClient.HasPermission(ctx, authHeader, authclient.PermissionCaller{
			Type:    caller.Type,
			UserID:  caller.UserID,
			Service: caller.Service,
			NodeID:  caller.NodeID,
			APIID:   caller.APIID,
		}, permissionCode, caller.ScopeType, caller.ScopeID)
	})
	routeTableOptions := routeTableOptionsFromConfig(c.Proxy)
	serviceStatusDriver := servicestatus.NewComposeDriver(routeTableOptions.TrustedServices, c.ServiceStatus.ComposeServices...)
	orchestratorClient := orchestratorsnapshot.NewClient(
		c.Orchestrator.Endpoint,
		c.Orchestrator.InternalToken,
		c.Orchestrator.ContributionAckToken,
	)
	topologyProjection := gtopology.NewStore(redisClient, serviceProxy)
	if err := topologyProjection.Recover(ctx); err != nil {
		return nil, fmt.Errorf("recover Gateway topology projections failed: %w", err)
	}
	var snapshot servicestatus.Snapshot
	if strings.TrimSpace(c.Orchestrator.NodeID) != "" {
		var table servicestatus.RouteTable
		if err := orchestratorClient.DecodeNodeOrchestratorRoutes(ctx, c.Orchestrator.NodeID, true, &table); err == nil {
			serviceProxy.SetRouteTable(table)
		} else {
			zlog.Warn("orchestrator node effective route table is unavailable; gateway starts degraded", zap.Error(err))
		}
	} else if err := orchestratorClient.DecodeOrchestratorSnapshot(ctx, false, &snapshot); err == nil {
		if services, serviceErr := serviceStatusDriver.ListServices(ctx, snapshot); serviceErr == nil {
			snapshot.Services = filterServiceStatusesByKind(services, false)
			snapshot.Workers = filterServiceStatusesByKind(services, true)
			routeTableOptions.ServiceStatuses = servicestatus.ServiceStatusesByID(snapshot.Services)
		}
		serviceProxy.SetRouteTable(servicestatus.BuildRouteTableWithOptions(snapshot, routeTableOptions))
	} else {
		zlog.Warn("orchestrator service snapshot is unavailable; gateway starts degraded", zap.Error(err))
	}
	*serviceContext = ServiceContext{
		Config:              c,
		Logger:              zlog,
		Redis:               redisClient,
		Tracer:              tp,
		Proxy:               serviceProxy.ServeHTTP,
		ServiceProxy:        serviceProxy,
		ServiceStatusDriver: serviceStatusDriver,
		RouteTableOptions:   routeTableOptions,
		InternalSigner:      internalSigner,
		Orchestrator:        orchestratorClient,
		AuthClient:          authClient,
		PermissionChecker:   permissionChecker,
		Context:             contextProvider,
		TopologyProjection:  topologyProjection,
	}
	if orchestratorClient.Configured() {
		if err := serviceContext.reloadContributionSnapshot(ctx); err != nil {
			zlog.Warn("orchestrator contribution snapshot is unavailable; contribution routes start degraded", zap.Error(err))
		}
		serviceContext.startContributionSnapshotReconciler(contributionSnapshotPollInterval)
	}
	return serviceContext, nil
}
