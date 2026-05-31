package proxy

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httputil"
	"net/url"
	"sort"
	"strconv"
	"strings"

	"ojos-gateway/internal/config"

	sharedjwt "ojos-shared/security/jwt"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	"go.uber.org/zap"
)

const (
	authModeNone     = "none"
	authModeOptional = "optional"
	authModeRequired = "required"
)

type claimsContextKey struct{}

type routeProxy struct {
	prefix   string
	authMode string
	proxy    *httputil.ReverseProxy
}

func NewConfigProxy(
	routes []config.ProxyRouteConfig,
	jwtSecret string,
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

		authMode, err := normalizeAuthMode(route.AuthMode)
		if err != nil {
			return nil, fmt.Errorf("invalid auth mode: prefix=%s: %w", prefix, err)
		}

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

				clearTrustedAuthHeaders(pr.Out.Header)

				if claims, ok := claimsFromContext(pr.In.Context()); ok && claims != nil {
					pr.Out.Header.Set("X-Auth-Verified", "true")
					pr.Out.Header.Set("X-User-Id", strconv.FormatInt(claims.UserID, 10))
					pr.Out.Header.Set("X-Username", claims.Username)
					pr.Out.Header.Set("X-Roles", strings.Join(claims.Roles, ","))
				} else {
					pr.Out.Header.Set("X-Auth-Verified", "false")
				}

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

				writeJSONError(w, http.StatusBadGateway, 50201, "bad gateway")
			},
		}

		compiled = append(compiled, routeProxy{
			prefix:   prefix,
			authMode: authMode,
			proxy:    rp,
		})
	}

	sort.SliceStable(compiled, func(i, j int) bool {
		return len(compiled[i].prefix) > len(compiled[j].prefix)
	})

	return func(w http.ResponseWriter, r *http.Request) {
		for _, route := range compiled {
			if matchPrefix(r.URL.Path, route.prefix) {
				claims, ok := authenticateRequest(w, r, route.authMode, jwtSecret)
				if !ok {
					return
				}

				if claims != nil {
					ctx := context.WithValue(r.Context(), claimsContextKey{}, claims)
					r = r.WithContext(ctx)
				}

				route.proxy.ServeHTTP(w, r)
				return
			}
		}

		http.NotFound(w, r)
	}, nil
}

func authenticateRequest(
	w http.ResponseWriter,
	r *http.Request,
	authMode string,
	jwtSecret string,
) (*sharedjwt.Claims, bool) {
	if authMode == authModeNone {
		return nil, true
	}

	authHeader := strings.TrimSpace(r.Header.Get("Authorization"))
	if authHeader == "" {
		if authMode == authModeOptional {
			return nil, true
		}

		writeJSONError(w, http.StatusUnauthorized, 40101, "missing authorization header")
		return nil, false
	}

	parts := strings.Fields(authHeader)
	if len(parts) != 2 || !strings.EqualFold(parts[0], "Bearer") {
		writeJSONError(w, http.StatusUnauthorized, 40102, "invalid authorization header")
		return nil, false
	}

	tokenString := strings.TrimSpace(parts[1])
	if tokenString == "" {
		writeJSONError(w, http.StatusUnauthorized, 40103, "empty token")
		return nil, false
	}

	claims, err := sharedjwt.Parse(jwtSecret, tokenString)
	if err != nil {
		writeJSONError(w, http.StatusUnauthorized, 40104, "invalid or expired token")
		return nil, false
	}

	return claims, true
}

func claimsFromContext(ctx context.Context) (*sharedjwt.Claims, bool) {
	claims, ok := ctx.Value(claimsContextKey{}).(*sharedjwt.Claims)
	return claims, ok
}

func clearTrustedAuthHeaders(header http.Header) {
	header.Del("X-Auth-Verified")
	header.Del("X-User-Id")
	header.Del("X-Username")
	header.Del("X-Roles")
}

func normalizeAuthMode(mode string) (string, error) {
	mode = strings.ToLower(strings.TrimSpace(mode))

	if mode == "" {
		return authModeNone, nil
	}

	switch mode {
	case authModeNone, authModeOptional, authModeRequired:
		return mode, nil
	default:
		return "", fmt.Errorf("unsupported auth mode: %s", mode)
	}
}

func writeJSONError(w http.ResponseWriter, httpStatus int, code int, msg string) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(httpStatus)

	_ = json.NewEncoder(w).Encode(map[string]any{
		"code": code,
		"msg":  msg,
	})
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
