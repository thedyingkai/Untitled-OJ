package handler

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"ojos-judge-api/internal/config"
	judgemw "ojos-judge-api/internal/middleware"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/zeromicro/go-zero/rest"
)

func TestWorkerClaimRouteOutlivesOrdinaryServerTimeout(t *testing.T) {
	endpoint, stop := startJudgeRouteTimeoutServer(t, 100*time.Millisecond, nil)
	defer stop()

	payload, err := json.Marshal(types.WorkerClaimTasksReq{
		WorkerId:           "worker-empty",
		SupportedLanguages: []string{"cpp17"},
		AvailableSlots:     1,
	})
	if err != nil {
		t.Fatal(err)
	}
	req, err := http.NewRequest(http.MethodPost, endpoint+"/api/judge/worker/tasks/claim", bytes.NewReader(payload))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-OJOS-Worker-Token", "worker-token")
	req.Header.Set("Prefer", "wait=25")

	started := time.Now()
	resp, err := (&http.Client{Timeout: 32 * time.Second}).Do(req)
	if err != nil {
		t.Fatalf("long poll request failed: %v", err)
	}
	defer resp.Body.Close()
	elapsed := time.Since(started)
	body, readErr := io.ReadAll(resp.Body)
	if readErr != nil {
		t.Fatalf("read long poll response: %v", readErr)
	}
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("long poll status=%d elapsed=%s body=%s", resp.StatusCode, elapsed, body)
	}
	if elapsed < 24*time.Second || elapsed > 30*time.Second {
		t.Fatalf("empty queue long poll elapsed=%s, want about 25s", elapsed)
	}
	if got := resp.Header.Get("Preference-Applied"); got != "wait=25" {
		t.Fatalf("Preference-Applied = %q", got)
	}
	var result types.WorkerClaimTasksResp
	if err := json.Unmarshal(body, &result); err != nil {
		t.Fatalf("decode long poll response: %v", err)
	}
	if len(result.Tasks) != 0 {
		t.Fatalf("empty queue returned tasks: %#v", result.Tasks)
	}
}

func TestOrdinaryJudgeRouteStillUsesGlobalTimeout(t *testing.T) {
	slowUserContext := func(next http.HandlerFunc) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) {
			select {
			case <-time.After(2 * time.Second):
				next(w, r)
			case <-r.Context().Done():
			}
		}
	}
	endpoint, stop := startJudgeRouteTimeoutServer(t, 100*time.Millisecond, slowUserContext)
	defer stop()

	started := time.Now()
	resp, err := http.Get(endpoint + "/judge/languages")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("ordinary route status=%d body=%s", resp.StatusCode, body)
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("ordinary route ignored its 100ms timeout: %s", elapsed)
	}
}

func TestWorkerClaimRouteTimeoutIsContractBounded(t *testing.T) {
	if workerClaimRouteTimeout != 35*time.Second {
		t.Fatalf("claim route timeout = %s", workerClaimRouteTimeout)
	}
}

func TestWorkerClaimTimeoutIsRecordedInAPISourceAndV2Compatibility(t *testing.T) {
	apiSource, err := os.ReadFile(filepath.Join("..", "..", "judgeapi.api"))
	if err != nil {
		t.Fatal(err)
	}
	apiText := string(apiSource)
	claim := strings.Index(apiText, "@handler workerClaimTasks")
	if claim < 0 {
		t.Fatal("judgeapi.api is missing workerClaimTasks")
	}
	server := strings.LastIndex(apiText[:claim], "@server (")
	if server < 0 {
		t.Fatal("workerClaimTasks has no @server declaration")
	}
	claimServer := apiText[server:claim]
	if !strings.Contains(claimServer, "timeout:    35s") {
		t.Fatalf("worker claim @server lost its 35s timeout:\n%s", claimServer)
	}
	if !strings.Contains(claimServer, "middleware: WorkerAuthMiddleware") {
		t.Fatalf("worker claim @server lost WorkerAuthMiddleware:\n%s", claimServer)
	}

	routeSource, err := os.ReadFile("routes.go")
	if err != nil {
		t.Fatal(err)
	}
	routeText := string(routeSource)
	if !strings.Contains(routeText, `registerWorkerRoutes(server, serverCtx, "/api/judge"`) {
		t.Fatal("generated route integration lost the Service Contract v2 /api/judge surface")
	}
	if !strings.Contains(routeText, "rest.WithTimeout(workerClaimRouteTimeout)") {
		t.Fatal("registered claim route lost its route-scoped timeout")
	}
}

func startJudgeRouteTimeoutServer(
	t *testing.T,
	globalTimeout time.Duration,
	userContext rest.Middleware,
) (string, func()) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	port := listener.Addr().(*net.TCPAddr).Port
	server, err := rest.NewServer(rest.RestConf{
		Host:    "127.0.0.1",
		Port:    port,
		Timeout: int64(globalTimeout / time.Millisecond),
		Middlewares: rest.MiddlewaresConf{
			Timeout: true,
		},
	})
	if err != nil {
		_ = listener.Close()
		t.Fatal(err)
	}
	_ = listener.Close()
	if userContext == nil {
		userContext = func(next http.HandlerFunc) http.HandlerFunc { return next }
	}
	workerAuth := judgemw.NewWorkerAuthMiddleware("worker-token", true)
	RegisterHandlers(server, &svc.ServiceContext{
		Config: config.Config{
			WorkerAuth: config.WorkerAuthConfig{
				Token:           "worker-token",
				LeaseTTLSeconds: 60,
			},
		},
		WorkerRepo:            &fakeWorkerHTTPRepo{},
		UserContextMiddleware: userContext,
		WorkerAuthMiddleware:  workerAuth.Handle,
	})
	go server.Start()
	endpoint := fmt.Sprintf("http://127.0.0.1:%d", port)
	waitForJudgeWorkerHTTPServer(t, endpoint)
	return endpoint, server.Stop
}
