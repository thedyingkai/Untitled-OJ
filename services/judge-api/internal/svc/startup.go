// Resource construction and startup rollback. No process termination here.
package svc

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/middleware"
	"ojos-judge-api/internal/repository"
	"ojos-problem-events/problemv1"
	"ojos-shared/database"
	"ojos-shared/eventing"
	sharedlogger "ojos-shared/logger"
	"ojos-shared/security/internalauth"
	sharedperm "ojos-shared/security/permission"
	"ojos-shared/security/workload"
	"ojos-shared/servicecontext"
	"ojos-shared/tracing"

	"github.com/redis/go-redis/v9"
	"go.uber.org/zap"
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
		return nil, fmt.Errorf("configure judge-api: %w", err)
	}
	if token := os.Getenv("OJOS_WORKER_TOKEN"); token != "" {
		c.WorkerAuth.Token = token
	}
	if leaseTTL := os.Getenv("OJOS_TASK_LEASE_TTL"); leaseTTL != "" && c.WorkerAuth.LeaseTTLSeconds <= 0 {
		if parsed, err := time.ParseDuration(leaseTTL + "s"); err == nil {
			c.WorkerAuth.LeaseTTLSeconds = int64(parsed.Seconds())
		}
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
		"judge-api",
		nil,
		[]eventing.EventSubscription{
			{EventType: problemv1.DeletedType, ConsumerGroup: "judge-api"},
			{EventType: problemv1.SnapshotType, ConsumerGroup: "judge-api"},
		},
	)
	if err != nil {
		return nil, fmt.Errorf("configure managed Event Contract failed: %w", err)
	}
	redisClient, err := newJudgeRedisClient(eventContext, c.Redis.Url)
	if err != nil {
		return nil, fmt.Errorf("configure judge redis client failed: %w", err)
	}
	svcCtx.Redis = redisClient
	if pingErr := probeJudgeRedis(ctx, redisClient); pingErr != nil {
		// Redis is an acceleration and event transport dependency, not the
		// durable Judge task authority. Starting without connectivity lets the
		// API persist submissions and lets Workers poll PostgreSQL; the existing
		// consumers and relays retry Redis in the background.
		zlog.Warn(
			"redis unavailable at startup; continuing with PostgreSQL judge task polling",
			zap.Error(pingErr),
		)
	}
	var eventRedis redis.UniversalClient = redisClient
	var contextProvider *servicecontext.ContextProvider
	var permissionChecker sharedperm.UserChecker
	contextValue, err := servicecontext.LoadOptional()
	if err != nil {
		return nil, fmt.Errorf("load managed Service Context failed: %w", err)
	}
	if contextValue != nil {
		if err := contextValue.RequireService("judge-api"); err != nil {
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
			for _, bindingName := range []string{storageGetBinding, storagePutBinding, storageHeadBinding} {
				var binding servicecontext.APIBinding
				binding, err = contextProvider.Binding(ctx, bindingName)
				if err != nil || binding.APIID != bindingName {
					err = fmt.Errorf("required API binding %s is unavailable", bindingName)
					break
				}
			}
		}
		if err == nil {
			err = contextProvider.Start(context.Background())
		}
		if err != nil {
			return nil, fmt.Errorf("configure managed ApiBindings failed: %w", err)
		}
	} else {
		if managedEnvironment() {
			return nil, errors.New("managed judge-api requires an Agent Service Context")
		}
		permissionChecker = sharedperm.NewUserCheckerWithConfig(permissionCheckerConfig(c), db)
		if permissionChecker == nil {
			return nil, errors.New("configure permission checker failed")
		}
	}
	c.Storage.SetContextProvider(contextProvider)

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

	repo := repository.New(db, repository.WithLegacyProblemPackageDir(c.ProblemProjection.AllowLegacyPackageDir))
	var workloadVerifier *workload.Verifier
	if strings.TrimSpace(c.WorkloadIdentity.PublicKeyFile) != "" {
		workloadVerifier, err = workload.NewVerifierFromPEMFile(
			c.WorkloadIdentity.PublicKeyFile,
			c.WorkloadIdentity.KeyID,
			c.WorkloadIdentity.Issuer,
			c.WorkloadIdentity.Audience,
		)
		if err != nil {
			return nil, fmt.Errorf("configure workload identity verifier failed: %w", err)
		}
	}
	if err := validateWorkerIdentityMode(
		c,
		workloadVerifier != nil,
		os.Getenv("OJOS_ENVIRONMENT"),
	); err != nil {
		return nil, fmt.Errorf("invalid Judge Worker identity configuration: %w", err)
	}
	if err := validateProblemProjectionMode(c, os.Getenv("OJOS_ENVIRONMENT")); err != nil {
		return nil, fmt.Errorf("invalid Problem projection configuration: %w", err)
	}
	workerAuthOptions := []any{}
	if workloadVerifier != nil {
		workerAuthOptions = append(workerAuthOptions, workloadVerifier)
	}
	// Explicitly pass the compatibility decision even when no verifier is
	// configured. This keeps development opt-in possible without making the
	// middleware constructor permissive by default.
	workerAuthOptions = append(workerAuthOptions, c.WorkloadIdentity.AllowLegacyWorkerToken)
	resultStream := strings.TrimSpace(os.Getenv("OJOS_JUDGE_RESULT_STREAM"))
	if resultStream == "" {
		resultStream = "ojos:judge:result"
	}
	*svcCtx = ServiceContext{
		Config:  c,
		Managed: managedEnvironment(),

		Logger: zlog,
		DB:     db,
		Tracer: tp,

		Repo:           repo,
		SubmissionRepo: repo,
		RejudgeRepo:    repo,
		WorkerRepo:     repo,
		Permission:     permissionChecker,
		Redis:          redisClient,
		Events:         eventContext,
		EventRedis:     eventRedis,
		ResultOutbox: &repository.JudgeResultOutboxRelay{
			DB:           db,
			Redis:        eventRedis,
			Stream:       resultStream,
			RelayID:      c.Name,
			PollInterval: 250 * time.Millisecond,
		},
		Context: contextProvider,

		UserContextMiddleware: middleware.NewUserContextMiddleware().Handle,
		InternalAuthMiddleware: middleware.NewInternalAuthMiddleware(
			c.InternalAuth.Enabled,
			internalVerifier,
		).Handle,
		WorkerAuthMiddleware: middleware.NewWorkerAuthMiddleware(c.WorkerAuth.Token, workerAuthOptions...).Handle,
	}
	if err := svcCtx.startProblemProjectionConsumer(); err != nil {
		return nil, err
	}
	return svcCtx, nil
}

func newJudgeRedisClient(eventContext *eventing.EventContext, redisURL string) (*redis.Client, error) {
	if eventContext != nil {
		// Judge queue wakeups and event projection share the Agent-approved
		// Redis connection. RedisClient validates the materialized connection
		// file and URL without requiring the endpoint to be reachable yet.
		client, err := eventContext.RedisClient()
		if err != nil {
			return nil, fmt.Errorf("load Agent-local event connection: %w", err)
		}
		return client, nil
	}

	redisURL = strings.TrimSpace(redisURL)
	if redisURL == "" {
		return nil, errors.New("redis url is required")
	}
	options, err := redis.ParseURL(redisURL)
	if err != nil {
		return nil, fmt.Errorf("parse redis url: %w", err)
	}
	return redis.NewClient(options), nil
}

func probeJudgeRedis(ctx context.Context, client *redis.Client) error {
	if client == nil {
		return errors.New("redis client is not configured")
	}
	probeCtx, cancel := context.WithTimeout(ctx, redisStartupProbeTimeout)
	defer cancel()
	return client.Ping(probeCtx).Err()
}
