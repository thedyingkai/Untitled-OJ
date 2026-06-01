package proxy

import (
	"net/http"

	"ojos-gateway/internal/config"

	"github.com/zeromicro/go-zero/rest"
)

func RegisterRoutes(
	server *rest.Server,
	routes []config.ProxyRouteConfig,
	handler http.HandlerFunc,
) {
	methods := []string{
		http.MethodGet,
		http.MethodPost,
		http.MethodPut,
		http.MethodPatch,
		http.MethodDelete,
		http.MethodOptions,
		http.MethodHead,
	}

	for _, route := range routes {
		prefix := cleanPrefix(route.Prefix)
		if prefix == "" {
			continue
		}

		paths := []string{
			prefix,
			prefix + "/:p1",
			prefix + "/:p1/:p2",
			prefix + "/:p1/:p2/:p3",
			prefix + "/:p1/:p2/:p3/:p4",
			prefix + "/:p1/:p2/:p3/:p4/:p5",
		}

		for _, method := range methods {
			for _, path := range paths {
				server.AddRoute(rest.Route{
					Method:  method,
					Path:    path,
					Handler: handler,
				})
			}
		}
	}
}
