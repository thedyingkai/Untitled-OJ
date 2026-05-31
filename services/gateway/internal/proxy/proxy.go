package proxy

import (
	"fmt"
	"net/http"
	"net/http/httputil"
	"net/url"
	"sort"
	"strings"

	"ojos-gateway/internal/config"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	"go.uber.org/zap"
)

type routeProxy struct {
	prefix      string
	stripPrefix string
	target      *url.URL
	proxy       *httputil.ReverseProxy
}

func NewConfigProxy(
	routes []config.ProxyRouteConfig,
	log *zap.Logger,
) (http.HandlerFunc, error) {
	if len(routes) == 0 {
		return http.NotFound, nil
	}

	compiled := make([]routeProxy, 0, len(routes))

	for _, route := range routes {
		if route.Prefix == "" {
			return nil, fmt.Errorf("proxy route prefix is empty")
		}

		if route.Target == "" {
			return nil, fmt.Errorf("proxy route target is empty: prefix=%s", route.Prefix)
		}

		target, err := url.Parse(route.Target)
		if err != nil {
			return nil, fmt.Errorf(
				"parse proxy target failed: prefix=%s target=%s: %w",
				route.Prefix,
				route.Target,
				err,
			)
		}

		prefix := cleanPrefix(route.Prefix)
		stripPrefix := cleanPrefix(route.StripPrefix)

		targetURL := target
		routePrefix := prefix
		routeStripPrefix := stripPrefix

		rp := &httputil.ReverseProxy{
			Rewrite: func(pr *httputil.ProxyRequest) {
				originalPath := pr.In.URL.Path
				originalQuery := pr.In.URL.RawQuery

				upstreamPath := originalPath
				if routeStripPrefix != "" {
					upstreamPath = strings.TrimPrefix(upstreamPath, routeStripPrefix)
				}

				if upstreamPath == "" {
					upstreamPath = "/"
				}

				if !strings.HasPrefix(upstreamPath, "/") {
					upstreamPath = "/" + upstreamPath
				}

				pr.Out.URL.Scheme = targetURL.Scheme
				pr.Out.URL.Host = targetURL.Host
				pr.Out.URL.Path = singleJoiningSlash(targetURL.Path, upstreamPath)
				pr.Out.URL.RawPath = ""
				pr.Out.URL.RawQuery = originalQuery

				pr.Out.Host = targetURL.Host
				pr.SetXForwarded()

				pr.Out.Header.Set("X-Forwarded-Prefix", routePrefix)
				pr.Out.Header.Set("X-Gateway", "ojos-gateway")

				otel.GetTextMapPropagator().Inject(
					pr.Out.Context(),
					propagation.HeaderCarrier(pr.Out.Header),
				)
			},

			ErrorHandler: func(w http.ResponseWriter, r *http.Request, err error) {
				log.Error(
					"gateway reverse proxy failed",
					zap.String("method", r.Method),
					zap.String("path", r.URL.Path),
					zap.String("target", targetURL.String()),
					zap.Error(err),
				)

				w.Header().Set("Content-Type", "application/json; charset=utf-8")
				w.WriteHeader(http.StatusBadGateway)
				_, _ = w.Write([]byte(`{"code":50201,"msg":"bad gateway"}`))
			},
		}

		compiled = append(compiled, routeProxy{
			prefix:      prefix,
			stripPrefix: stripPrefix,
			target:      target,
			proxy:       rp,
		})
	}

	sort.SliceStable(compiled, func(i, j int) bool {
		return len(compiled[i].prefix) > len(compiled[j].prefix)
	})

	return func(w http.ResponseWriter, r *http.Request) {
		for _, route := range compiled {
			if matchPrefix(r.URL.Path, route.prefix) {
				route.proxy.ServeHTTP(w, r)
				return
			}
		}

		http.NotFound(w, r)
	}, nil
}

func cleanPrefix(prefix string) string {
	prefix = strings.TrimSpace(prefix)

	if prefix == "" {
		return ""
	}

	if !strings.HasPrefix(prefix, "/") {
		prefix = "/" + prefix
	}

	if len(prefix) > 1 {
		prefix = strings.TrimRight(prefix, "/")
	}

	return prefix
}

func matchPrefix(path string, prefix string) bool {
	return path == prefix || strings.HasPrefix(path, prefix+"/")
}

func singleJoiningSlash(a string, b string) string {
	aslash := strings.HasSuffix(a, "/")
	bslash := strings.HasPrefix(b, "/")

	switch {
	case aslash && bslash:
		return a + b[1:]
	case !aslash && !bslash:
		return a + "/" + b
	default:
		return a + b
	}
}
