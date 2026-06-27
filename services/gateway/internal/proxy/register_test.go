package proxy

import (
	"net/http"
	"testing"

	"ojos-gateway/internal/config"

	"github.com/zeromicro/go-zero/rest"
)

func TestRegisterRoutesAllowsDynamicFallbackWithStaticCoreRoutes(t *testing.T) {
	server := rest.MustNewServer(rest.RestConf{
		Host: "127.0.0.1",
		Port: 0,
	})
	defer server.Stop()

	RegisterRoutes(server, []config.ProxyRouteConfig{
		{Prefix: "/api/auth", Target: "http://auth:8081", StripPrefix: "/api", AuthMode: "optional"},
		{Prefix: "/api/problem", Target: "http://problem-api:8083", StripPrefix: "/api", AuthMode: "required"},
	}, http.NotFound)
}
