package logic

import (
	"context"
	"errors"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type AdminHealthLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewAdminHealthLogic(ctx context.Context, svcCtx *svc.ServiceContext) *AdminHealthLogic {
	return &AdminHealthLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *AdminHealthLogic) AdminHealth(authHeader string) (*types.AdminHealthResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}

	components := []types.HealthComponent{
		component("gateway", time.Now(), nil),
		l.checkPostgres(),
		l.checkRedis(),
		l.checkArtifactStorage(),
		l.checkInternalAuthKey(),
	}

	for _, route := range l.svcCtx.Config.Proxy.Routes {
		if route.Prefix == "/api/auth" || route.Prefix == "/api/problem" || route.Prefix == "/api/judge" {
			components = append(components, l.checkHTTP(route))
		}
	}
	if strings.TrimSpace(l.svcCtx.Config.Installer.Endpoint) != "" {
		components = append(components, l.checkInstaller())
	}

	workerOnline := l.workerOnlineCount()
	queuePending := l.queuePendingCount()
	components = append(components, l.checkWorkers(workerOnline))
	components = append(components, l.checkQueue(queuePending))

	overall := "ok"
	for _, c := range components {
		if c.Status != "ok" {
			overall = "degraded"
			break
		}
	}

	return &types.AdminHealthResp{
		Status:            overall,
		Components:        components,
		WorkerOnlineCount: workerOnline,
		QueuePending:      queuePending,
		InternalAuth:      statusFromBool(l.svcCtx.Config.InternalAuth.Enabled),
	}, nil
}

func (l *AdminHealthLogic) checkPostgres() types.HealthComponent {
	start := time.Now()
	err := l.svcCtx.DB.Ping(l.ctx)
	return component("postgres", start, err)
}

func (l *AdminHealthLogic) checkRedis() types.HealthComponent {
	start := time.Now()
	err := l.svcCtx.Redis.Ping(l.ctx).Err()
	return component("redis", start, err)
}

func (l *AdminHealthLogic) checkArtifactStorage() types.HealthComponent {
	start := time.Now()
	err := checkDirReadable(l.svcCtx.Config.Storage.ProblemsRoot, "problems artifact root")
	if err == nil {
		err = checkDirReadable(l.svcCtx.Config.Storage.SubmissionsRoot, "submissions artifact root")
	}
	return component("artifact storage", start, err)
}

func (l *AdminHealthLogic) checkInternalAuthKey() types.HealthComponent {
	start := time.Now()
	if !l.svcCtx.Config.InternalAuth.Enabled {
		c := component("internal auth key", start, nil)
		c.Message = "disabled"
		return c
	}

	var active int64
	err := l.svcCtx.DB.QueryRow(l.ctx, `
SELECT COUNT(*)
FROM internal_auth_keys
WHERE not_before <= NOW()
  AND verify_until >= NOW()
`).Scan(&active)
	if err == nil && active == 0 {
		err = errors.New("no active internal auth verification key")
	}
	c := component("internal auth key", start, err)
	if err == nil {
		c.Message = "active"
	}
	return c
}

func (l *AdminHealthLogic) checkHTTP(route config.ProxyRouteConfig) types.HealthComponent {
	start := time.Now()
	name := strings.TrimPrefix(route.Prefix, "/api/")
	client := http.Client{Timeout: 2 * time.Second}
	resp, err := client.Get(strings.TrimRight(route.Target, "/") + "/health")
	if err == nil && resp != nil {
		_ = resp.Body.Close()
		if resp.StatusCode >= 400 {
			err = errors.New(resp.Status)
		}
	}
	return component(name, start, err)
}

func (l *AdminHealthLogic) checkInstaller() types.HealthComponent {
	start := time.Now()
	client := http.Client{Timeout: 2 * time.Second}
	resp, err := client.Get(strings.TrimRight(l.svcCtx.Config.Installer.Endpoint, "/") + "/health")
	if err == nil && resp != nil {
		_ = resp.Body.Close()
		if resp.StatusCode >= 400 {
			err = errors.New(resp.Status)
		}
	}
	return component("module-installer", start, err)
}

func (l *AdminHealthLogic) workerOnlineCount() int64 {
	var count int64
	if err := l.svcCtx.DB.QueryRow(l.ctx, `
SELECT COUNT(*)
FROM judge_workers
WHERE status IN ('ONLINE', 'DRAINING')
  AND last_seen > NOW() - interval '120 seconds'
`).Scan(&count); err != nil {
		return -1
	}
	return count
}

func (l *AdminHealthLogic) queuePendingCount() int64 {
	var count int64
	if err := l.svcCtx.DB.QueryRow(l.ctx, `
SELECT COUNT(*)
FROM judge_tasks
WHERE status = 'PENDING'
`).Scan(&count); err != nil {
		return -1
	}
	return count
}

func (l *AdminHealthLogic) checkWorkers(count int64) types.HealthComponent {
	start := time.Now()
	var err error
	if count < 0 {
		err = errors.New("worker status query failed")
	}
	c := component("workers", start, err)
	if err == nil {
		c.Message = "online=" + strconv.FormatInt(count, 10)
	}
	return c
}

func (l *AdminHealthLogic) checkQueue(pending int64) types.HealthComponent {
	start := time.Now()
	var err error
	if pending < 0 {
		err = errors.New("queue status query failed")
	}
	c := component("queue", start, err)
	if err == nil {
		c.Message = "pending=" + strconv.FormatInt(pending, 10)
	}
	return c
}

func component(name string, start time.Time, err error) types.HealthComponent {
	c := types.HealthComponent{
		Name:    name,
		Status:  "ok",
		Latency: time.Since(start).Milliseconds(),
	}
	if err != nil {
		c.Status = "error"
		c.Message = err.Error()
	}
	return c
}

func statusFromBool(ok bool) string {
	if ok {
		return "ok"
	}
	return "error"
}

func checkDirReadable(path string, label string) error {
	path = strings.TrimSpace(path)
	if path == "" {
		return errors.New(label + " is not configured")
	}
	stat, err := os.Stat(path)
	if err != nil {
		return err
	}
	if !stat.IsDir() {
		return errors.New(label + " is not a directory")
	}
	entries, err := os.ReadDir(path)
	if err != nil {
		return err
	}
	_ = entries
	return nil
}
