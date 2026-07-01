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

	seenPaths := map[string]bool{}
	add := func(method string, path string) {
		key := method + " " + path
		if seenPaths[key] {
			return
		}
		seenPaths[key] = true
		server.AddRoute(rest.Route{
			Method:  method,
			Path:    path,
			Handler: handler,
		})
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
				add(method, path)
			}
		}
	}

	dynamicPaths := []string{
		"/internal/apis/:api",
		"/internal/apis/:api/:p1",
		"/internal/apis/:api/:p1/:p2",
		"/internal/apis/:api/:p1/:p2/:p3",
		"/internal/apis/:api/:p1/:p2/:p3/:p4",
		"/internal/apis/:api/:p1/:p2/:p3/:p4/:p5",
		"/internal/apis/:api/:p1/:p2/:p3/:p4/:p5/:p6",
		"/internal/apis/:api/:p1/:p2/:p3/:p4/:p5/:p6/:p7",
		"/internal/apis/:api/:p1/:p2/:p3/:p4/:p5/:p6/:p7/:p8",
		"/api/:p1",
		"/api/:p1/:p2",
		"/api/:p1/:p2/:p3",
		"/api/:p1/:p2/:p3/:p4",
		"/api/:p1/:p2/:p3/:p4/:p5",
		"/api/:p1/:p2/:p3/:p4/:p5/:p6",
		"/api/:p1/:p2/:p3/:p4/:p5/:p6/:p7",
		"/api/:p1/:p2/:p3/:p4/:p5/:p6/:p7/:p8",
	}
	for _, method := range methods {
		for _, path := range dynamicPaths {
			add(method, path)
		}
	}
}
