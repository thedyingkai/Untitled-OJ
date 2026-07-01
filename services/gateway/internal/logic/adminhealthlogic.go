package logic

import (
	"context"
	"encoding/json"
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

func (l *AdminHealthLogic) AdminHealth(req *types.AdminAuthReq) (*types.AdminHealthResp, error) {
	return l.adminHealth(req.Authorization)
}

func (l *AdminHealthLogic) adminHealth(authHeader string) (*types.AdminHealthResp, error) {
	if err := requireAdmin(l.ctx, l.svcCtx, authHeader); err != nil {
		return nil, err
	}

	components := []types.HealthComponent{
		component("gateway", time.Now(), nil),
		l.checkRedis(),
		l.checkArtifactStorage(),
	}

	for _, route := range l.svcCtx.Config.Proxy.Routes {
		if route.Prefix == "/api/auth" || route.Prefix == "/api/problem" || route.Prefix == "/api/judge" {
			components = append(components, l.checkHTTP(route))
		}
	}
	if strings.TrimSpace(l.svcCtx.Config.Orchestrator.Endpoint) != "" {
		components = append(components, l.checkOrchestrator())
	}
	components = append(components, l.runtimeHealthChecks()...)

	workerOnline, queuePending, judgeAdminComponents := l.judgeAdminStatus(authHeader)
	components = append(components, judgeAdminComponents...)
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

func (l *AdminHealthLogic) checkOrchestrator() types.HealthComponent {
	start := time.Now()
	client := http.Client{Timeout: 2 * time.Second}
	resp, err := client.Get(strings.TrimRight(l.svcCtx.Config.Orchestrator.Endpoint, "/") + "/health")
	if err == nil && resp != nil {
		_ = resp.Body.Close()
		if resp.StatusCode >= 400 {
			err = errors.New(resp.Status)
		}
	}
	return component("ojos-orchestrator", start, err)
}

func (l *AdminHealthLogic) runtimeHealthChecks() []types.HealthComponent {
	if l.svcCtx == nil || l.svcCtx.Orchestrator == nil || !l.svcCtx.Orchestrator.Configured() {
		return nil
	}
	var snapshot struct {
		HealthChecks []runtimeHealthCheck `json:"health_checks"`
	}
	err := l.svcCtx.Orchestrator.DecodeOrchestratorSnapshot(l.ctx, false, &snapshot)
	if err != nil {
		c := component("orchestrator health snapshot", time.Now(), nil)
		c.Status = "warning"
		c.Message = "orchestrator health snapshot unavailable"
		return []types.HealthComponent{c}
	}
	out := make([]types.HealthComponent, 0, len(snapshot.HealthChecks))
	for _, check := range snapshot.HealthChecks {
		name := "service:" + check.ServiceID + "/" + check.ComponentID
		c := component(name, time.Now(), nil)
		c.Message = runtimeHealthMessage(check)
		out = append(out, c)
	}
	return out
}

type runtimeHealthCheck struct {
	ServiceID   string          `json:"service_id"`
	ComponentID string          `json:"component_id"`
	Type        string          `json:"type"`
	Config      json.RawMessage `json:"config"`
}

func runtimeHealthMessage(check runtimeHealthCheck) string {
	var config struct {
		Type     string `json:"type"`
		Optional bool   `json:"optional"`
		Target   string `json:"target"`
	}
	_ = json.Unmarshal(check.Config, &config)
	checkType := strings.TrimSpace(config.Type)
	if checkType == "" {
		checkType = "metadata"
	}
	optional := "required"
	if config.Optional {
		optional = "optional"
	}
	if config.Target != "" {
		return checkType + " " + optional + " registered target=" + config.Target
	}
	return checkType + " " + optional + " registered"
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

func (l *AdminHealthLogic) judgeAdminStatus(authHeader string) (int64, int64, []types.HealthComponent) {
	judgeBase := l.routeTarget("/api/judge")
	if judgeBase == "" {
		c := component("judge admin api", time.Now(), errors.New("judge route target is not configured"))
		return -1, -1, []types.HealthComponent{c}
	}

	workers, workersComponent := l.fetchJudgeWorkers(judgeBase, authHeader)
	queuePending, queueComponent := l.fetchJudgeQueuePending(judgeBase, authHeader)
	return workers, queuePending, []types.HealthComponent{workersComponent, queueComponent}
}

func (l *AdminHealthLogic) routeTarget(prefix string) string {
	if l == nil || l.svcCtx == nil {
		return ""
	}
	for _, route := range l.svcCtx.Config.Proxy.Routes {
		if route.Prefix == prefix {
			return strings.TrimRight(strings.TrimSpace(route.Target), "/")
		}
	}
	return ""
}

func (l *AdminHealthLogic) fetchJudgeWorkers(baseURL string, authHeader string) (int64, types.HealthComponent) {
	start := time.Now()
	var payload struct {
		Workers []struct {
			Status string `json:"status"`
		} `json:"workers"`
	}
	err := l.fetchJudgeAdminJSON(baseURL, "/judge/admin/workers", authHeader, &payload)
	if err != nil {
		return -1, component("judge workers api", start, err)
	}
	var online int64
	for _, worker := range payload.Workers {
		if worker.Status == "ONLINE" || worker.Status == "DRAINING" {
			online++
		}
	}
	c := component("judge workers api", start, nil)
	c.Message = "online=" + strconv.FormatInt(online, 10)
	return online, c
}

func (l *AdminHealthLogic) fetchJudgeQueuePending(baseURL string, authHeader string) (int64, types.HealthComponent) {
	start := time.Now()
	var payload struct {
		Pending int64 `json:"pending"`
	}
	err := l.fetchJudgeAdminJSON(baseURL, "/judge/admin/queue", authHeader, &payload)
	if err != nil {
		return -1, component("judge queue api", start, err)
	}
	c := component("judge queue api", start, nil)
	c.Message = "pending=" + strconv.FormatInt(payload.Pending, 10)
	return payload.Pending, c
}

func (l *AdminHealthLogic) fetchJudgeAdminJSON(baseURL string, path string, authHeader string, out any) error {
	client := http.Client{Timeout: 2 * time.Second}
	req, err := http.NewRequestWithContext(l.ctx, http.MethodGet, strings.TrimRight(baseURL, "/")+path, nil)
	if err != nil {
		return err
	}
	if strings.TrimSpace(authHeader) != "" {
		req.Header.Set("Authorization", strings.TrimSpace(authHeader))
	}
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode >= 400 {
		return errors.New(resp.Status)
	}
	return json.NewDecoder(resp.Body).Decode(out)
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
