package config

import (
	"path/filepath"
	"testing"

	"github.com/zeromicro/go-zero/core/conf"
)

func TestPlatformBootstrapGatewayConfigHasOnlyAuthStaticRoute(t *testing.T) {
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
	if len(cfg.Proxy.Routes) != 1 {
		t.Fatalf("bootstrap static routes = %#v, want one Auth platform route", cfg.Proxy.Routes)
	}
	route := cfg.Proxy.Routes[0]
	if route.Prefix != "/api/auth" || route.Target != "http://auth-service:8081" || route.AuthMode != "optional" {
		t.Fatalf("bootstrap Auth route = %#v", route)
	}
	if len(cfg.Proxy.TrustedServices) != 1 || cfg.Proxy.TrustedServices[0].ServiceID != "auth-service" {
		t.Fatalf("bootstrap trusted services = %#v, want Auth only", cfg.Proxy.TrustedServices)
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
