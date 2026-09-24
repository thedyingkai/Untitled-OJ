// Resource construction and startup rollback. No process termination here.
package svc

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"ojos-problem-events/problemv1"
	"ojos-problem-service/internal/config"
	"ojos-problem-service/internal/middleware"
	"ojos-problem-service/internal/packagemutation"
	"ojos-problem-service/internal/repository"
	"ojos-shared/database"
	"ojos-shared/eventing"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/security/internalauth"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/servicecontext"
	"ojos-shared/tracing"

	"github.com/redis/go-redis/v9"
)

func NewServiceContext(c config.Config) (result *ServiceContext, startupErr error) {
	svcCtx := &ServiceContext{}
	defer func() {
		if result == nil {
			shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			svcCtx.Close(shutdownCtx)
		}
	}()
	ctx := context.Background()
	if err := applyEnvOverrides(&c); err != nil {
		return nil, fmt.Errorf("configure problem-service: %w", err)
	}

	zlog, err := sharedlogger.New(c.Name)
	if err != nil {
		return nil, fmt.Errorf("init logger failed: %w", err)
	}
	svcCtx.Logger = zlog

	tp, err := tracing.InitOTLP(ctx, c.Name, c.Jaeger.Endpoint)
	if err != nil {
		return nil, fmt.Errorf("init tracing failed: %w", err)
	}
	svcCtx.Tracer = tp

	db, err := database.NewPostgresPoolByURL(ctx, c.Database.Url)
	if err != nil {
		return nil, fmt.Errorf("connect postgres failed: %w", err)
	}
	svcCtx.DB = db

	eventContext, err := eventing.LoadEventContextForService(
		"problem-service",
		[]string{problemv1.DeletedType, problemv1.SnapshotType},
		nil,
	)
	if err != nil {
		return nil, fmt.Errorf("configure managed Event Contract failed: %w", err)
	}
	var redisClient *redis.Client
	if eventContext != nil {
		managedRedis, managedErr := eventContext.RedisClient()
		if managedErr != nil {
			return nil, fmt.Errorf("load Agent-local event connection failed: %w", managedErr)
		}
		svcCtx.Redis = managedRedis
		if managedErr = managedRedis.Ping(ctx).Err(); managedErr != nil {
			return nil, fmt.Errorf("ping Agent-local event connection failed: %w", managedErr)
		}
		// In a managed deployment the Agent-local connection is the only
		// Redis credential delivered to the process. Reuse it for nonce state
		// and event relay instead of requiring an undeclared REDIS_URL.
		redisClient = managedRedis
	} else {
		redisOptions, parseErr := redis.ParseURL(c.Redis.Url)
		if parseErr != nil {
			return nil, fmt.Errorf("parse redis url failed: %w", parseErr)
		}
		redisClient = redis.NewClient(redisOptions)
		svcCtx.Redis = redisClient
		if pingErr := redisClient.Ping(ctx).Err(); pingErr != nil {
			return nil, fmt.Errorf("ping redis failed: %w", pingErr)
		}
	}
	var eventRedis redis.UniversalClient = redisClient
	var contextProvider *servicecontext.ContextProvider
	var permissionChecker sharedperm.UserChecker
	contextValue, err := servicecontext.LoadOptional()
	if err != nil {
		return nil, fmt.Errorf("load managed Service Context failed: %w", err)
	}
	if contextValue != nil {
		if err := contextValue.RequireService("problem-service"); err != nil {
			return nil, fmt.Errorf("validate managed Service Context failed: %w", err)
		}
		contextPath := strings.TrimSpace(os.Getenv("OJOS_SERVICE_CONTEXT_FILE"))
		if contextPath == "" {
			contextPath = servicecontext.DefaultFile
		}
		contextProvider, err = servicecontext.NewContextProvider(contextPath, servicecontext.ProviderOptions{})
		svcCtx.Context = contextProvider
		if err == nil {
			permissionChecker, err = sharedperm.NewContextProviderUserChecker(contextProvider, permissionBindingName)
		}
		if err == nil {
			err = contextProvider.Start(context.Background())
		}
		if err != nil {
			return nil, fmt.Errorf("configure permission ApiBinding failed: %w", err)
		}
	} else {
		if managedEnvironment() {
			return nil, errors.New("managed problem-service requires an Agent Service Context")
		}
		permissionChecker = sharedperm.NewUserCheckerWithConfig(permissionCheckerConfig(c), db)
		if permissionChecker == nil {
			return nil, errors.New("configure permission checker failed")
		}
	}

	internalAuthCfg := internalauth.Config{
		Enabled:       c.InternalAuth.Enabled,
		TimestampSkew: time.Duration(c.InternalAuth.TimestampSkewSeconds) * time.Second,
		NonceTTL:      time.Duration(c.InternalAuth.NonceTTLSeconds) * time.Second,
	}

	var internalVerifier *internalauth.Verifier
	if c.InternalAuth.Enabled {
		internalKeyManager := internalauth.NewKeyManager(db, internalAuthCfg)
		internalNonceStore := internalauth.RedisNonceStore{
			Client: redisClient,
			Prefix: "ojos:internal-auth:nonce:",
		}

		internalVerifier = internalauth.NewVerifier(
			internalKeyManager,
			internalNonceStore,
			internalAuthCfg,
		)
	}

	*svcCtx = ServiceContext{
		Config:  c,
		Managed: managedEnvironment(),

		Logger:     zlog,
		DB:         db,
		Tracer:     tp,
		Redis:      redisClient,
		Events:     eventContext,
		EventRedis: eventRedis,

		Repo:       repository.New(db),
		Permission: permissionChecker,
		Context:    contextProvider,

		InternalAuthMiddleware: middleware.NewInternalAuthMiddleware(
			c.InternalAuth.Enabled,
			internalVerifier,
		).Handle,
		UserContextMiddleware: middleware.NewUserContextMiddleware().Handle,
	}
	if err := packagemutation.RecoverAll(ctx, svcCtx.Repo, c.Storage.ProblemsRoot); err != nil {
		return nil, fmt.Errorf("recover problem package mutation journal failed: %w", err)
	}
	if err := svcCtx.startProjectionBackground(); err != nil {
		return nil, err
	}
	return svcCtx, nil
}
