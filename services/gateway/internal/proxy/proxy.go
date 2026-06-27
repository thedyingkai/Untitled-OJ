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
	"sync/atomic"
	"time"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/kernel/moduleruntime"
	"ojos-shared/security/internalauth"

	sharedjwt "ojos-shared/security/jwt"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	"go.uber.org/zap"
)

const (
	authModeNone     = "none"
	authModeOptional = "optional"
	authModeRequired = "required"
	authModePublic   = "public"
	authModeUser     = "user"
	authModeAdmin    = "admin"
	authModeWorker   = "worker"
	authModeInternal = "internal"
)

type claimsContextKey struct{}

type routeProxy struct {
	prefix        string
	serviceID     string
	authMode      string
	stripPrefix   string
	rewritePrefix string
	proxy         *httputil.ReverseProxy
	target        *url.URL
}

type RuntimeReader interface {
	RuntimeRouteTable(context.Context) (moduleruntime.RouteTable, error)
}

type RuntimeProxy struct {
	jwtSecret      string
	internalSigner *internalauth.Signer
	adminChecker   AdminChecker
	log            *zap.Logger
	staticRoutes   []routeProxy
	trusted        map[string]trustedService
	table          atomic.Value
}

type AdminChecker func(context.Context, int64) (bool, error)

type trustedService struct {
	serviceID     string
	target        *url.URL
	stripPrefix   string
	rewritePrefix string
	healthCheckID string
}

func NewConfigProxy(
	routes []config.ProxyRouteConfig,
	trustedServices []config.ProxyTrustedServiceConfig,
	jwtSecret string,
	internalSigner *internalauth.Signer,
	log *zap.Logger,
) (http.HandlerFunc, error) {
	runtimeProxy, err := NewRuntimeProxy(routes, trustedServices, jwtSecret, internalSigner, log)
	if err != nil {
		return nil, err
	}
	return runtimeProxy.ServeHTTP, nil
}

func NewRuntimeProxy(
	routes []config.ProxyRouteConfig,
	trustedServices []config.ProxyTrustedServiceConfig,
	jwtSecret string,
	internalSigner *internalauth.Signer,
	log *zap.Logger,
) (*RuntimeProxy, error) {
	compiled := make([]routeProxy, 0, len(routes))
	trusted, err := compileTrustedServices(routes, trustedServices)
	if err != nil {
		return nil, err
	}

	for _, route := range routes {
		if route.Prefix == "" {
			return nil, fmt.Errorf("static proxy route prefix is empty")
		}

		if route.Target == "" {
			return nil, fmt.Errorf("static proxy route target is empty: prefix=%s", route.Prefix)
		}

		target, err := url.Parse(route.Target)
		if err != nil {
			return nil, fmt.Errorf(
				"parse static proxy target failed: prefix=%s target=%s: %w",
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

		compiled = append(compiled, routeProxy{
			prefix:      routePrefix,
			authMode:    authMode,
			stripPrefix: routeStripPrefix,
			proxy:       newReverseProxy(targetURL, routePrefix, routeStripPrefix, "", shouldForwardStaticAuthorization(routePrefix), internalSigner, log),
			target:      targetURL,
		})
	}

	sort.SliceStable(compiled, func(i, j int) bool {
		return len(compiled[i].prefix) > len(compiled[j].prefix)
	})

	runtimeProxy := &RuntimeProxy{
		jwtSecret:      jwtSecret,
		internalSigner: internalSigner,
		log:            log,
		staticRoutes:   compiled,
		trusted:        trusted,
	}
	runtimeProxy.table.Store(moduleruntime.RouteTable{Version: "0"})
	return runtimeProxy, nil
}

func (p *RuntimeProxy) SetAdminChecker(checker AdminChecker) {
	p.adminChecker = checker
}

func (p *RuntimeProxy) Reload(ctx context.Context, reader RuntimeReader) (moduleruntime.RouteTable, error) {
	table, err := reader.RuntimeRouteTable(ctx)
	if err != nil {
		return moduleruntime.RouteTable{}, err
	}
	p.SetRouteTable(table)
	return table, nil
}

func (p *RuntimeProxy) SetRouteTable(table moduleruntime.RouteTable) {
	p.table.Store(table)
}

func (p *RuntimeProxy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	for _, route := range p.staticRoutes {
		if isCoreStaticProxyPrefix(route.prefix) && matchPrefix(r.URL.Path, route.prefix) {
			p.serveRoute(w, r, route)
			return
		}
	}

	if route, ok := p.matchRuntimeRoute(r.URL.Path); ok {
		p.serveRoute(w, r, route)
		return
	}

	for _, route := range p.staticRoutes {
		if !isCoreStaticProxyPrefix(route.prefix) && matchPrefix(r.URL.Path, route.prefix) {
			p.serveRoute(w, r, route)
			return
		}
	}

	http.NotFound(w, r)
}

func (p *RuntimeProxy) matchRuntimeRoute(path string) (routeProxy, bool) {
	value := p.table.Load()
	table, _ := value.(moduleruntime.RouteTable)
	for _, route := range table.Routes {
		if !route.ProxyEnabled || !matchPrefix(path, route.Prefix) {
			continue
		}
		service, ok := p.trusted[route.ServiceID]
		if !ok {
			continue
		}
		return routeProxy{
			prefix:        route.Prefix,
			serviceID:     route.ServiceID,
			authMode:      route.AuthMode,
			stripPrefix:   firstNonEmpty(route.StripPrefix, service.stripPrefix),
			rewritePrefix: firstNonEmpty(route.RewritePrefix, service.rewritePrefix),
			proxy: newReverseProxy(
				service.target,
				route.Prefix,
				firstNonEmpty(route.StripPrefix, service.stripPrefix),
				firstNonEmpty(route.RewritePrefix, service.rewritePrefix),
				false,
				p.internalSigner,
				p.log,
			),
			target: service.target,
		}, true
	}
	return routeProxy{}, false
}

func (p *RuntimeProxy) serveRoute(w http.ResponseWriter, r *http.Request, route routeProxy) {
	claims, ok := p.authenticateRequest(w, r, route.authMode)
	if !ok {
		return
	}

	if claims != nil {
		ctx := context.WithValue(r.Context(), claimsContextKey{}, claims)
		r = r.WithContext(ctx)
	}

	route.proxy.ServeHTTP(w, r)
}

func (p *RuntimeProxy) authenticateRequest(
	w http.ResponseWriter,
	r *http.Request,
	authMode string,
) (*sharedjwt.Claims, bool) {
	authMode = normalizeRuntimeAuthMode(authMode)
	if authMode == authModePublic {
		return nil, true
	}
	if authMode == authModeInternal {
		writeJSONError(w, http.StatusForbidden, 40303, "internal route is not public")
		return nil, false
	}
	if authMode == authModeWorker {
		writeJSONError(w, http.StatusForbidden, 40304, "worker route is not available through dynamic proxy")
		return nil, false
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

	claims, err := sharedjwt.Parse(p.jwtSecret, tokenString)
	if err != nil {
		writeJSONError(w, http.StatusUnauthorized, 40104, "invalid or expired token")
		return nil, false
	}

	if authMode == authModeAdmin {
		if isAdminRole(claims.Roles) {
			return claims, true
		}
		if p.adminChecker != nil {
			ok, err := p.adminChecker(r.Context(), claims.UserID)
			if err != nil {
				writeJSONError(w, http.StatusInternalServerError, 50001, "authorization check failed")
				return nil, false
			}
			if ok {
				return claims, true
			}
		}
		writeJSONError(w, http.StatusForbidden, 40301, "forbidden")
		return nil, false
	}

	return claims, true
}

func claimsFromContext(ctx context.Context) (*sharedjwt.Claims, bool) {
	claims, ok := ctx.Value(claimsContextKey{}).(*sharedjwt.Claims)
	return claims, ok
}

func normalizeAuthMode(mode string) (string, error) {
	mode = normalizeRuntimeAuthMode(mode)
	switch mode {
	case authModePublic, authModeOptional, authModeUser, authModeAdmin, authModeWorker, authModeInternal:
		return mode, nil
	default:
		return "", fmt.Errorf("unsupported auth mode: %s", mode)
	}
}

func normalizeRuntimeAuthMode(mode string) string {
	mode = strings.ToLower(strings.TrimSpace(mode))
	switch mode {
	case "", authModeNone, "public":
		return authModePublic
	case authModeRequired, "user":
		return authModeUser
	case authModeOptional:
		return authModeOptional
	case "admin":
		return authModeAdmin
	case "worker":
		return authModeWorker
	case "internal":
		return authModeInternal
	default:
		return mode
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

func compileTrustedServices(
	routes []config.ProxyRouteConfig,
	trustedServices []config.ProxyTrustedServiceConfig,
) (map[string]trustedService, error) {
	out := map[string]trustedService{}
	for _, item := range trustedServices {
		serviceID := strings.TrimSpace(item.ServiceID)
		if serviceID == "" {
			return nil, fmt.Errorf("trusted service id is empty")
		}
		service, err := compileTrustedService(serviceID, item.Target, item.StripPrefix, item.RewritePrefix, item.HealthCheckID)
		if err != nil {
			return nil, err
		}
		out[serviceID] = service
	}

	// Compatibility: existing static routes become trusted services when no explicit map exists.
	for _, route := range routes {
		serviceID := inferServiceID(route.Target)
		if serviceID == "" {
			continue
		}
		if _, ok := out[serviceID]; ok {
			continue
		}
		service, err := compileTrustedService(serviceID, route.Target, route.StripPrefix, "", "")
		if err != nil {
			return nil, err
		}
		out[serviceID] = service
	}
	return out, nil
}

func compileTrustedService(serviceID string, target string, stripPrefix string, rewritePrefix string, healthCheckID string) (trustedService, error) {
	targetURL, err := url.Parse(strings.TrimSpace(target))
	if err != nil {
		return trustedService{}, fmt.Errorf("parse trusted service target failed: service=%s: %w", serviceID, err)
	}
	if targetURL.Scheme != "http" && targetURL.Scheme != "https" {
		return trustedService{}, fmt.Errorf("trusted service target scheme is not allowed: service=%s", serviceID)
	}
	if strings.TrimSpace(targetURL.Host) == "" {
		return trustedService{}, fmt.Errorf("trusted service target host is empty: service=%s", serviceID)
	}
	return trustedService{
		serviceID:     serviceID,
		target:        targetURL,
		stripPrefix:   cleanPrefix(stripPrefix),
		rewritePrefix: cleanPrefix(rewritePrefix),
		healthCheckID: strings.TrimSpace(healthCheckID),
	}, nil
}

func inferServiceID(target string) string {
	targetURL, err := url.Parse(strings.TrimSpace(target))
	if err != nil || targetURL.Hostname() == "" {
		return ""
	}
	return targetURL.Hostname()
}

func newReverseProxy(
	targetURL *url.URL,
	routePrefix string,
	stripPrefix string,
	rewritePrefix string,
	forwardAuthorization bool,
	internalSigner *internalauth.Signer,
	log *zap.Logger,
) *httputil.ReverseProxy {
	return &httputil.ReverseProxy{
		Rewrite: func(pr *httputil.ProxyRequest) {
			originalPath := pr.In.URL.Path
			originalQuery := pr.In.URL.RawQuery

			upstreamPath := originalPath
			if stripPrefix != "" {
				upstreamPath = strings.TrimPrefix(upstreamPath, stripPrefix)
			}
			if rewritePrefix != "" {
				upstreamPath = singleJoiningSlash(rewritePrefix, upstreamPath)
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
			removeHopByHopHeaders(pr.Out.Header)
			if !forwardAuthorization {
				pr.Out.Header.Del("Authorization")
			}
			internalauth.ClearTrustedAuthHeaders(pr.Out.Header)
			internalauth.ClearInternalAuthHeaders(pr.Out.Header)

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
			pr.Out.Header.Set("X-OJOS-Gateway-Proxy", "runtime")

			if internalSigner != nil {
				if err := internalSigner.SignRequest(pr.Out.Context(), pr.Out); err != nil {
					log.Error(
						"gateway internal auth sign failed",
						zap.String("method", pr.In.Method),
						zap.String("path", pr.In.URL.Path),
						zap.String("target_host", targetURL.Host),
						zap.Error(err),
					)

					pr.Out.Header.Set("X-OJOS-Internal-Sign-Error", "true")
				}
			}

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
				zap.String("target_host", targetURL.Host),
				zap.Error(err),
			)

			writeJSONError(w, http.StatusBadGateway, 50201, "bad gateway")
		},
		Transport: &http.Transport{
			Proxy:                 http.ProxyFromEnvironment,
			ResponseHeaderTimeout: 15 * time.Second,
		},
	}
}

func shouldForwardStaticAuthorization(prefix string) bool {
	switch cleanPrefix(prefix) {
	case "/api/auth", "/api/judge/worker":
		return true
	default:
		return false
	}
}

func isCoreStaticProxyPrefix(prefix string) bool {
	switch cleanPrefix(prefix) {
	case "/api/auth", "/api/judge/worker":
		return true
	default:
		return false
	}
}

func removeHopByHopHeaders(header http.Header) {
	for _, key := range []string{
		"Connection",
		"Proxy-Connection",
		"Keep-Alive",
		"Proxy-Authenticate",
		"Proxy-Authorization",
		"TE",
		"Trailer",
		"Transfer-Encoding",
		"Upgrade",
	} {
		header.Del(key)
	}
}

func isAdminRole(roles []string) bool {
	for _, role := range roles {
		if role == "admin" || role == "super_admin" {
			return true
		}
	}
	return false
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return value
		}
	}
	return ""
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
