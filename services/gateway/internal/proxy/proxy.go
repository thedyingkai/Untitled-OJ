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
	"ojos-gateway/internal/orchestrator/servicestatus"
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

	internalAPIPrefix = "/internal/apis"
)

type claimsContextKey struct{}

type routeProxy struct {
	prefix             string
	apiID              string
	callerNodeID       string
	providerNodeID     string
	providerService    string
	providerEndpoint   string
	serviceID          string
	authMode           string
	requiredPermission string
	stripPrefix        string
	rewritePrefix      string
	proxy              *httputil.ReverseProxy
	target             *url.URL
}

type ServiceRouteReader interface {
	ServiceRouteTable(context.Context) (servicestatus.RouteTable, error)
}

type ServiceProxy struct {
	jwtSecret         string
	internalSigner    *internalauth.Signer
	adminChecker      AdminChecker
	permissionChecker PermissionChecker
	log               *zap.Logger
	staticRoutes      []routeProxy
	trusted           map[string]trustedService
	nodeID            string
	table             atomic.Value
}

type AdminChecker func(context.Context, string, int64) (bool, error)

type PermissionChecker func(context.Context, string, int64, string) (bool, error)

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
	serviceProxy, err := NewServiceProxy(routes, trustedServices, jwtSecret, internalSigner, log)
	if err != nil {
		return nil, err
	}
	return serviceProxy.ServeHTTP, nil
}

func NewServiceProxy(
	routes []config.ProxyRouteConfig,
	trustedServices []config.ProxyTrustedServiceConfig,
	jwtSecret string,
	internalSigner *internalauth.Signer,
	log *zap.Logger,
) (*ServiceProxy, error) {
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

	serviceProxy := &ServiceProxy{
		jwtSecret:      jwtSecret,
		internalSigner: internalSigner,
		log:            log,
		staticRoutes:   compiled,
		trusted:        trusted,
	}
	serviceProxy.table.Store(servicestatus.RouteTable{Version: "0"})
	return serviceProxy, nil
}

func (p *ServiceProxy) SetAdminChecker(checker AdminChecker) {
	p.adminChecker = checker
}

func (p *ServiceProxy) SetPermissionChecker(checker PermissionChecker) {
	p.permissionChecker = checker
}

func (p *ServiceProxy) SetNodeID(nodeID string) {
	p.nodeID = strings.TrimSpace(nodeID)
}

func (p *ServiceProxy) Reload(ctx context.Context, reader ServiceRouteReader) (servicestatus.RouteTable, error) {
	table, err := reader.ServiceRouteTable(ctx)
	if err != nil {
		return servicestatus.RouteTable{}, err
	}
	p.SetRouteTable(table)
	return table, nil
}

func (p *ServiceProxy) SetRouteTable(table servicestatus.RouteTable) {
	p.table.Store(table)
}

func (p *ServiceProxy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	for _, route := range p.staticRoutes {
		if isCoreStaticProxyPrefix(route.prefix) && matchPrefix(r.URL.Path, route.prefix) {
			p.serveRoute(w, r, route)
			return
		}
	}

	if matchPrefix(r.URL.Path, internalAPIPrefix) {
		p.serveInternalAPI(w, r)
		return
	}

	if route, ok := p.matchServiceRoute(r.URL.Path); ok {
		p.serveRoute(w, r, route)
		return
	}
	if blocked, ok := p.matchBlockedServiceRoute(r.URL.Path); ok {
		if _, ok := p.authenticateRequest(w, r, blocked.authMode); !ok {
			return
		}
		writeJSONError(w, http.StatusServiceUnavailable, 50301, "service unavailable: "+blocked.serviceID)
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

func (p *ServiceProxy) matchBlockedServiceRoute(path string) (routeProxy, bool) {
	value := p.table.Load()
	table, _ := value.(servicestatus.RouteTable)
	for _, route := range table.Routes {
		if route.ProxyEnabled || !route.Enabled || !matchPrefix(path, route.Prefix) {
			continue
		}
		if route.Status == "unavailable" || containsString(route.BlockedBy, "service not running") || containsString(route.BlockedBy, "service degraded") {
			return routeProxy{prefix: route.Prefix, serviceID: route.ServiceID, authMode: route.AuthMode}, true
		}
	}
	return routeProxy{}, false
}

func (p *ServiceProxy) matchServiceRoute(path string) (routeProxy, bool) {
	value := p.table.Load()
	table, _ := value.(servicestatus.RouteTable)
	for _, route := range table.Routes {
		if !route.ProxyEnabled || !matchPrefix(path, route.Prefix) {
			continue
		}
		target, stripPrefix, rewritePrefix, ok := p.routeTarget(route)
		if !ok {
			continue
		}
		return routeProxy{
			prefix:             route.Prefix,
			serviceID:          route.ServiceID,
			authMode:           route.AuthMode,
			requiredPermission: normalizeRequiredPermission(route.RequiredPermission),
			stripPrefix:        stripPrefix,
			rewritePrefix:      rewritePrefix,
			proxy: newReverseProxy(
				target,
				route.Prefix,
				stripPrefix,
				rewritePrefix,
				false,
				p.internalSigner,
				p.log,
			),
			target: target,
		}, true
	}
	return routeProxy{}, false
}

func (p *ServiceProxy) serveInternalAPI(w http.ResponseWriter, r *http.Request) {
	apiID, _, ok := internalAPIRequest(r.URL.Path)
	if !ok {
		http.NotFound(w, r)
		return
	}
	callerNodeID := strings.TrimSpace(r.Header.Get("X-OJOS-Node-Id"))
	if callerNodeID == "" {
		callerNodeID = p.nodeID
	}
	if callerNodeID == "" {
		writeJSONError(w, http.StatusBadRequest, 40001, "caller node id is required")
		return
	}

	route, found, unavailable := p.matchInternalAPIRoute(apiID, r.Method)
	if unavailable {
		writeJSONError(w, http.StatusServiceUnavailable, 50302, "api route not available: "+apiID)
		return
	}
	if !found {
		http.NotFound(w, r)
		return
	}
	route.callerNodeID = callerNodeID
	r = cloneRequestWithResolverHeaders(r, route)
	p.serveRoute(w, r, route)
}

func (p *ServiceProxy) matchInternalAPIRoute(apiID string, method string) (routeProxy, bool, bool) {
	value := p.table.Load()
	table, _ := value.(servicestatus.RouteTable)
	hasUnavailable := false
	for _, route := range table.Routes {
		if strings.TrimSpace(route.ApiID) != apiID {
			continue
		}
		if !methodAllowed(method, route.Methods) {
			continue
		}
		if !route.ProxyEnabled {
			hasUnavailable = true
			continue
		}
		target, ok := routeUpstreamBaseTarget(route)
		if !ok {
			hasUnavailable = true
			continue
		}
		providerService := firstNonEmpty(route.ProviderService, route.ServiceID, route.TargetService)
		providerEndpoint := strings.TrimSpace(route.ProviderEndpoint)
		return routeProxy{
			prefix:             cleanPrefix(route.Prefix),
			apiID:              apiID,
			providerNodeID:     strings.TrimSpace(route.ProviderNodeID),
			providerService:    providerService,
			providerEndpoint:   providerEndpoint,
			serviceID:          firstNonEmpty(route.ServiceID, providerService),
			authMode:           route.AuthMode,
			requiredPermission: normalizeRequiredPermission(route.RequiredPermission),
			stripPrefix:        internalAPIPrefix + "/" + apiID,
			rewritePrefix:      cleanPrefix(route.Prefix),
			proxy: newReverseProxy(
				target,
				internalAPIPrefix+"/"+apiID,
				internalAPIPrefix+"/"+apiID,
				cleanPrefix(route.Prefix),
				false,
				p.internalSigner,
				p.log,
			),
			target: target,
		}, true, false
	}
	return routeProxy{}, false, hasUnavailable
}

func cloneRequestWithResolverHeaders(r *http.Request, route routeProxy) *http.Request {
	cloned := r.Clone(r.Context())
	cloned.Header = r.Header.Clone()
	cloned.Header.Set("X-OJOS-Node-Id", route.callerNodeID)
	cloned.Header.Set("X-OJOS-Resolved-Provider-Node-Id", route.providerNodeID)
	cloned.Header.Set("X-OJOS-Resolved-Provider-Service", route.providerService)
	cloned.Header.Set("X-OJOS-Resolved-Provider-Endpoint", route.providerEndpoint)
	return cloned
}

func routeUpstreamBaseTarget(route servicestatus.ServiceRoute) (*url.URL, bool) {
	target, err := url.Parse(strings.TrimSpace(route.UpstreamBase))
	if err != nil || (target.Scheme != "http" && target.Scheme != "https") || target.Host == "" {
		return nil, false
	}
	return target, true
}

func (p *ServiceProxy) routeTarget(route servicestatus.ServiceRoute) (*url.URL, string, string, bool) {
	if strings.TrimSpace(route.UpstreamBase) != "" {
		target, err := url.Parse(strings.TrimSpace(route.UpstreamBase))
		if err == nil && (target.Scheme == "http" || target.Scheme == "https") && target.Host != "" {
			return target, cleanPrefix(route.StripPrefix), cleanPrefix(route.RewritePrefix), true
		}
	}
	service, ok := p.trusted[route.ServiceID]
	if !ok {
		return nil, "", "", false
	}
	return service.target,
		firstNonEmpty(route.StripPrefix, service.stripPrefix),
		firstNonEmpty(route.RewritePrefix, service.rewritePrefix),
		true
}

func containsString(items []string, want string) bool {
	for _, item := range items {
		if item == want {
			return true
		}
	}
	return false
}

func (p *ServiceProxy) serveRoute(w http.ResponseWriter, r *http.Request, route routeProxy) {
	authMode := route.authMode
	if route.requiredPermission != "" && normalizeServiceAuthMode(authMode) == authModePublic {
		authMode = authModeUser
	}
	claims, ok := p.authenticateRequest(w, r, authMode)
	if !ok {
		return
	}

	if !p.authorizeRequiredPermission(w, r, claims, route.requiredPermission) {
		return
	}

	if claims != nil {
		ctx := context.WithValue(r.Context(), claimsContextKey{}, claims)
		r = r.WithContext(ctx)
	}

	route.proxy.ServeHTTP(w, r)
}

func (p *ServiceProxy) authorizeRequiredPermission(
	w http.ResponseWriter,
	r *http.Request,
	claims *sharedjwt.Claims,
	requiredPermission string,
) bool {
	requiredPermission = normalizeRequiredPermission(requiredPermission)
	if requiredPermission == "" {
		return true
	}
	if claims == nil {
		writeJSONError(w, http.StatusUnauthorized, 40105, "missing authorization claims")
		return false
	}
	if p.permissionChecker == nil {
		writeJSONError(w, http.StatusInternalServerError, 50002, "permission check unavailable")
		return false
	}
	ok, err := p.permissionChecker(
		r.Context(),
		strings.TrimSpace(r.Header.Get("Authorization")),
		claims.UserID,
		requiredPermission,
	)
	if err != nil {
		writeJSONError(w, http.StatusInternalServerError, 50003, "permission check failed")
		return false
	}
	if !ok {
		writeJSONError(w, http.StatusForbidden, 40305, "permission denied")
		return false
	}
	return true
}

func (p *ServiceProxy) authenticateRequest(
	w http.ResponseWriter,
	r *http.Request,
	authMode string,
) (*sharedjwt.Claims, bool) {
	authMode = normalizeServiceAuthMode(authMode)
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
			ok, err := p.adminChecker(
				r.Context(),
				strings.TrimSpace(r.Header.Get("Authorization")),
				claims.UserID,
			)
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
	mode = normalizeServiceAuthMode(mode)
	switch mode {
	case authModePublic, authModeOptional, authModeUser, authModeAdmin, authModeWorker, authModeInternal:
		return mode, nil
	default:
		return "", fmt.Errorf("unsupported auth mode: %s", mode)
	}
}

func normalizeServiceAuthMode(mode string) string {
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

func normalizeRequiredPermission(permission string) string {
	permission = strings.TrimSpace(permission)
	if strings.EqualFold(permission, "public") {
		return ""
	}
	return permission
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

func internalAPIRequest(path string) (string, string, bool) {
	if !matchPrefix(path, internalAPIPrefix) {
		return "", "", false
	}
	rest := strings.TrimPrefix(path, internalAPIPrefix)
	rest = strings.TrimPrefix(rest, "/")
	if rest == "" {
		return "", "", false
	}
	apiID, tail, _ := strings.Cut(rest, "/")
	apiID = strings.TrimSpace(apiID)
	if apiID == "" {
		return "", "", false
	}
	return apiID, "/" + tail, true
}

func methodAllowed(method string, allowed []string) bool {
	if len(allowed) == 0 {
		return true
	}
	method = strings.ToUpper(strings.TrimSpace(method))
	for _, item := range allowed {
		item = strings.ToUpper(strings.TrimSpace(item))
		if item == method || item == "ANY" || item == "*" {
			return true
		}
		if method == http.MethodHead && item == http.MethodGet {
			return true
		}
	}
	return false
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
			pr.Out.Header.Set("X-OJOS-Gateway-Proxy", "service-routing")
			if apiID, _, ok := internalAPIRequest(originalPath); ok {
				pr.Out.Header.Set("X-OJOS-Api-Id", apiID)
				if callerNodeID := strings.TrimSpace(pr.In.Header.Get("X-OJOS-Node-Id")); callerNodeID != "" {
					pr.Out.Header.Set("X-OJOS-Caller-Node-Id", callerNodeID)
				}
				if providerNodeID := strings.TrimSpace(pr.In.Header.Get("X-OJOS-Resolved-Provider-Node-Id")); providerNodeID != "" {
					pr.Out.Header.Set("X-OJOS-Provider-Node-Id", providerNodeID)
				}
				if providerService := strings.TrimSpace(pr.In.Header.Get("X-OJOS-Resolved-Provider-Service")); providerService != "" {
					pr.Out.Header.Set("X-OJOS-Provider-Service", providerService)
				}
				if providerEndpoint := strings.TrimSpace(pr.In.Header.Get("X-OJOS-Resolved-Provider-Endpoint")); providerEndpoint != "" {
					pr.Out.Header.Set("X-OJOS-Provider-Endpoint", providerEndpoint)
				}
				pr.Out.Header.Del("X-OJOS-Resolved-Provider-Node-Id")
				pr.Out.Header.Del("X-OJOS-Resolved-Provider-Service")
				pr.Out.Header.Del("X-OJOS-Resolved-Provider-Endpoint")
			}

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
