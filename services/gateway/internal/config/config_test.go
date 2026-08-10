package config

import (
	"path/filepath"
	"testing"

	"github.com/zeromicro/go-zero/core/conf"
)

func TestProductionGatewayConfigStreamsWithBoundedRoutes(t *testing.T) {
	var cfg Config
	if err := conf.Load(filepath.Join("..", "..", "etc", "gateway.yaml"), &cfg); err != nil {
		t.Fatal(err)
	}
	if cfg.Timeout < 600000 {
		t.Fatalf("Gateway network timeout = %dms, want at least 600000ms", cfg.Timeout)
	}
	if cfg.Middlewares.Timeout {
		t.Fatal("go-zero timeout middleware must stay disabled for streaming proxy responses")
	}
	if cfg.Middlewares.Recover {
		t.Fatal("native recovery must not swallow http.ErrAbortHandler from ReverseProxy")
	}
	foundWorker := false
	for _, route := range cfg.Proxy.Routes {
		if route.Prefix == "/api/judge/worker" {
			foundWorker = true
			if route.TimeoutMS < 35000 {
				t.Fatalf("legacy worker route timeout = %dms, want at least 35000ms", route.TimeoutMS)
			}
		}
	}
	if !foundWorker {
		t.Fatal("legacy worker route is missing")
	}
}

func TestPrepareProxyServerOverridesGeneratedThreeSecondDefault(t *testing.T) {
	cfg := Config{}
	cfg.Timeout = 3000
	cfg.Middlewares.Timeout = true
	cfg.Middlewares.Recover = true

	cfg.PrepareProxyServer()

	if cfg.Timeout != minimumProxyNetworkTimeoutMS {
		t.Fatalf("network timeout = %dms", cfg.Timeout)
	}
	if cfg.Middlewares.Timeout || cfg.Middlewares.Recover {
		t.Fatalf("buffering/native recovery remained enabled: %#v", cfg.Middlewares)
	}
}
