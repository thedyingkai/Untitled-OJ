package middleware

import (
	"net/http"

	"github.com/prometheus/client_golang/prometheus/promhttp"
	"github.com/zeromicro/go-zero/rest"
)

// RegisterMetricsRoute exposes the process registry on the service's own
// listener. It is intentionally unauthenticated on the private application
// network so Prometheus can scrape it without sharing an application token.
func RegisterMetricsRoute(server *rest.Server) {
	server.AddRoute(rest.Route{
		Method:  http.MethodGet,
		Path:    "/metrics",
		Handler: promhttp.Handler().ServeHTTP,
	})
}
