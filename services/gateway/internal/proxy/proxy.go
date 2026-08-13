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
	"sync"
	"sync/atomic"
	"time"

	"ojos-gateway/internal/config"
	"ojos-gateway/internal/orchestrator/servicestatus"
	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-shared/security/internalauth"
	"ojos-shared/security/workload"

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
	authModeService  = "service"
	authModeWorkload = "workload"

	internalAPIPrefix          = "/internal/apis"
	contributionAudienceHeader = "X-OJOS-Audience"
)

type claimsContextKey struct{}
type workloadClaimsContextKey struct{}
type matchedPathParametersContextKey struct{}

type routeProxy struct {
	prefix               string
	apiID                string
	operationID          string
	deploymentID         string
	revisionID           string
	generation           uint64
	audience             string
	pathTemplate         string
	providerPath         string
	bindingID            string
	consumerDeploymentID string
	consumerServiceID    string
	consumerNodeID       string
	credentialGeneration uint64
	timeoutMS            uint64
	callerNodeID         string
	providerNodeID       string
	providerService      string
	providerEndpoint     string
	serviceID            string
	authMode             string
	providerAuthMode     string
	requiredPermission   string
	permissionScope      *servicestatus.PermissionScope
	stripPrefix          string
	rewritePrefix        string
	proxy                *httputil.ReverseProxy
	target               *url.URL
}

type compiledPublishedRoute struct {
	source        servicestatus.ServiceRoute
	service       routeProxy
	serviceReady  bool
	internal      routeProxy
	internalReady bool
}

// compiledRouteTable is immutable after publication. A request acquires one
// snapshot for its complete lifetime, so a revision switch is atomic and the
// retired transports are released only after the last old request exits.
type compiledRouteTable struct {
	table   servicestatus.RouteTable
	routes  []compiledPublishedRoute
	mu      sync.Mutex
	users   int
	retired bool
	close   sync.Once
}

type ServiceRouteReader interface {
	ServiceRouteTable(context.Context) (servicestatus.RouteTable, error)
}

type ServiceProxy struct {
	jwtSecret          string
	internalSigner     *internalauth.Signer
	adminChecker       AdminChecker
	permissionChecker  PermissionChecker
	log                *zap.Logger
	staticRoutes       []routeProxy
	trusted            map[string]trustedService
	nodeID             string
	workloadVerifier   *workload.Verifier
	tableMu            sync.Mutex
	baseTable          servicestatus.RouteTable
	topologyTable      servicestatus.RouteTable
	contributionTable  servicestatus.RouteTable
	table              atomic.Pointer[compiledRouteTable]
	closed             atomic.Bool
	extensionArtifacts *extensionArtifactRegistry
}

type RouteTableValidationError struct {
	RouteID string
	Reason  string
}

func (e *RouteTableValidationError) Error() string {
	return "invalid contribution route " + e.RouteID + ": " + e.Reason
}

type AdminChecker func(context.Context, string, int64) (bool, error)

type PermissionCheckCaller struct {
	Type      string
	UserID    int64
	Service   string
	NodeID    string
	APIID     string
	ScopeType string
	ScopeID   int64
}

type PermissionChecker func(context.Context, string, PermissionCheckCaller, string) (bool, error)

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
		totalTimeout := routeTotalTimeout(route.TimeoutMS, staticRouteFallbackTimeout(routePrefix))

		compiled = append(compiled, routeProxy{
			prefix:      routePrefix,
			authMode:    authMode,
			stripPrefix: routeStripPrefix,
			timeoutMS:   durationMilliseconds(totalTimeout),
			proxy:       newReverseProxy(targetURL, routePrefix, routeStripPrefix, "", shouldForwardStaticAuthorization(routePrefix), totalTimeout, internalSigner, log),
			target:      targetURL,
		})
	}

	sort.SliceStable(compiled, func(i, j int) bool {
		return len(compiled[i].prefix) > len(compiled[j].prefix)
	})

	serviceProxy := &ServiceProxy{
		jwtSecret:          jwtSecret,
		internalSigner:     internalSigner,
		log:                log,
		staticRoutes:       compiled,
		trusted:            trusted,
		extensionArtifacts: newExtensionArtifactRegistry(),
	}
	serviceProxy.baseTable = servicestatus.RouteTable{Version: "0"}
	serviceProxy.topologyTable = servicestatus.RouteTable{Version: "0"}
	serviceProxy.contributionTable = servicestatus.RouteTable{Version: "0"}
	serviceProxy.table.Store(serviceProxy.compileRouteTable(servicestatus.RouteTable{Version: "0"}))
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

func (p *ServiceProxy) SetWorkloadVerifier(verifier *workload.Verifier) {
	p.workloadVerifier = verifier
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
	p.tableMu.Lock()
	if p.closed.Load() {
		p.tableMu.Unlock()
		return
	}
	p.baseTable = cloneRouteTable(table)
	previous := p.rebuildRouteTableLocked()
	p.tableMu.Unlock()
	previous.retire()
}

// SetTopologyRouteTable atomically replaces only the control-plane-owned
// ApiBinding routes. Snapshot reloads and topology apply can therefore race
// without either source erasing the other.
func (p *ServiceProxy) SetTopologyRouteTable(table servicestatus.RouteTable) {
	p.tableMu.Lock()
	if p.closed.Load() {
		p.tableMu.Unlock()
		return
	}
	p.topologyTable = cloneRouteTable(table)
	previous := p.rebuildRouteTableLocked()
	p.tableMu.Unlock()
	previous.retire()
}

// SetContributionRouteTable atomically replaces deployment-scoped external
// operation routes while preserving legacy registry routes and ApiBindings.
func (p *ServiceProxy) SetContributionRouteTable(table servicestatus.RouteTable) {
	_ = p.TrySetContributionRouteTable(table)
}

// TrySetContributionRouteTable compiles a candidate before publication. A bad
// revision therefore cannot evict the currently active, healthy snapshot.
func (p *ServiceProxy) TrySetContributionRouteTable(table servicestatus.RouteTable) error {
	if err := validateContributionRouteTable(table); err != nil {
		return err
	}
	p.tableMu.Lock()
	if p.closed.Load() {
		p.tableMu.Unlock()
		return nil
	}
	p.contributionTable = cloneRouteTable(table)
	previous := p.rebuildRouteTableLocked()
	p.tableMu.Unlock()
	previous.retire()
	return nil
}

// ApplyContributionSnapshot validates routes and frontend artifacts as one
// candidate before atomically publishing the route revision and allowlist.
func (p *ServiceProxy) ApplyContributionSnapshot(table servicestatus.RouteTable, snapshot orchestratorsnapshot.ContributionSnapshot) error {
	if p == nil || p.extensionArtifacts == nil {
		return fmt.Errorf("contribution snapshot consumer is unavailable")
	}
	if err := validateContributionRouteTable(table); err != nil {
		return err
	}
	artifacts, err := compileExtensionArtifacts(snapshot)
	if err != nil {
		return err
	}
	p.tableMu.Lock()
	if p.closed.Load() {
		p.tableMu.Unlock()
		return nil
	}
	p.extensionArtifacts.replaceCompiled(artifacts)
	p.contributionTable = cloneRouteTable(table)
	previous := p.rebuildRouteTableLocked()
	p.tableMu.Unlock()
	previous.retire()
	return nil
}

func (p *ServiceProxy) SetContributionArtifacts(snapshot orchestratorsnapshot.ContributionSnapshot) error {
	if p == nil || p.extensionArtifacts == nil {
		return fmt.Errorf("extension artifact registry is unavailable")
	}
	return p.extensionArtifacts.replace(snapshot)
}

func validateContributionRouteTable(table servicestatus.RouteTable) error {
	for _, route := range table.Routes {
		if route.CreatedFrom != "contribution_snapshot_v1" {
			continue
		}
		if !route.ProxyEnabled {
			continue
		}
		if _, ok := routeUpstreamBaseTarget(route); !ok {
			return &RouteTableValidationError{RouteID: route.RouteID, Reason: "invalid upstream_base"}
		}
		params, ok := templateParameterNames(route.PathTemplate)
		if !ok {
			return &RouteTableValidationError{RouteID: route.RouteID, Reason: "invalid external path template"}
		}
		providerParams, ok := templateParameterNames(route.ProviderPath)
		if !ok || !equalStringSet(params, providerParams) {
			return &RouteTableValidationError{RouteID: route.RouteID, Reason: "provider path parameters differ from external path"}
		}
		if normalizeRequiredPermission(route.RequiredPermission) == "" && route.PermissionScope != nil {
			return &RouteTableValidationError{RouteID: route.RouteID, Reason: "permission scope has no permission"}
		}
		if normalizeRequiredPermission(route.RequiredPermission) != "" {
			scope := route.PermissionScope
			if scope != nil && scope.Kind != "system" && (scope.Kind != "path_parameter" || scope.Type == "" || !params[scope.PathParameter] || !providerParams[scope.PathParameter]) {
				return &RouteTableValidationError{RouteID: route.RouteID, Reason: "invalid permission scope"}
			}
		}
	}
	return nil
}

func templateParameterNames(path string) (map[string]bool, bool) {
	segments, ok := splitTemplatePath(path)
	if !ok {
		return nil, false
	}
	out := make(map[string]bool)
	for _, segment := range segments {
		if name, parameter := templateParameter(segment); parameter {
			out[name] = true
		}
	}
	return out, true
}

func equalStringSet(left, right map[string]bool) bool {
	if len(left) != len(right) {
		return false
	}
	for value := range left {
		if !right[value] {
			return false
		}
	}
	return true
}

func (p *ServiceProxy) rebuildRouteTableLocked() *compiledRouteTable {
	merged := cloneRouteTable(p.baseTable)
	projectionIDs := make(map[string]bool, len(p.topologyTable.Routes))
	projectionBindings := make(map[string]bool, len(p.topologyTable.Routes))
	for _, route := range p.topologyTable.Routes {
		projectionIDs[route.RouteID] = true
		if route.BindingID != "" {
			projectionBindings[route.BindingID] = true
		}
	}
	filtered := merged.Routes[:0]
	for _, route := range merged.Routes {
		if route.CreatedFrom == "topology_binding_v1" || projectionIDs[route.RouteID] || projectionBindings[route.BindingID] {
			continue
		}
		filtered = append(filtered, route)
	}
	merged.Routes = append(filtered, cloneServiceRoutes(p.topologyTable.Routes)...)
	merged.Routes = append(merged.Routes, cloneServiceRoutes(p.contributionTable.Routes)...)
	sort.SliceStable(merged.Routes, func(i, j int) bool {
		if merged.Routes[i].Priority == merged.Routes[j].Priority {
			return merged.Routes[i].RouteID < merged.Routes[j].RouteID
		}
		return merged.Routes[i].Priority > merged.Routes[j].Priority
	})
	if p.topologyTable.Version != "" && p.topologyTable.Version != "0" {
		merged.Version = p.topologyTable.Version
		merged.GeneratedAt = p.topologyTable.GeneratedAt
	}
	merged.CanProxy = p.baseTable.CanProxy || p.topologyTable.CanProxy
	merged.CanProxy = merged.CanProxy || p.contributionTable.CanProxy
	merged.Warnings = append(append(append([]string(nil), p.baseTable.Warnings...), p.topologyTable.Warnings...), p.contributionTable.Warnings...)
	if p.contributionTable.Version != "" && p.contributionTable.Version != "0" {
		merged.Version = p.contributionTable.Version
	}
	return p.table.Swap(p.compileRouteTable(merged))
}

func (p *ServiceProxy) compileRouteTable(table servicestatus.RouteTable) *compiledRouteTable {
	cloned := cloneRouteTable(table)
	compiled := &compiledRouteTable{
		table:  cloned,
		routes: make([]compiledPublishedRoute, 0, len(cloned.Routes)),
	}
	for _, route := range cloned.Routes {
		entry := compiledPublishedRoute{source: route}
		if route.ProxyEnabled {
			entry.service, entry.serviceReady = p.compileServiceRoute(route)
			entry.internal, entry.internalReady = p.compileInternalRoute(route)
		}
		compiled.routes = append(compiled.routes, entry)
	}
	return compiled
}

func (p *ServiceProxy) compileServiceRoute(route servicestatus.ServiceRoute) (routeProxy, bool) {
	target, stripPrefix, rewritePrefix, ok := p.routeTarget(route)
	if !ok {
		return routeProxy{}, false
	}
	totalTimeout := routeTotalTimeout(route.TimeoutMS, 30*time.Second)
	compiled := routeProxy{
		prefix:               route.Prefix,
		apiID:                strings.TrimSpace(route.ApiID),
		operationID:          strings.TrimSpace(route.OperationID),
		deploymentID:         strings.TrimSpace(route.DeploymentID),
		revisionID:           strings.TrimSpace(route.RevisionID),
		generation:           route.Generation,
		audience:             strings.ToLower(strings.TrimSpace(route.Audience)),
		pathTemplate:         cleanPrefix(route.PathTemplate),
		providerPath:         cleanPrefix(route.ProviderPath),
		bindingID:            route.BindingID,
		consumerDeploymentID: route.ConsumerDeploymentID,
		consumerServiceID:    route.ConsumerServiceID,
		consumerNodeID:       route.ConsumerNodeID,
		credentialGeneration: route.CredentialGeneration,
		timeoutMS:            durationMilliseconds(totalTimeout),
		serviceID:            route.ServiceID,
		authMode:             route.AuthMode,
		providerAuthMode:     route.ProviderAuthMode,
		requiredPermission:   normalizeRequiredPermission(route.RequiredPermission),
		permissionScope:      clonePermissionScope(route.PermissionScope),
		stripPrefix:          stripPrefix,
		rewritePrefix:        rewritePrefix,
		target:               target,
	}
	forwardAuthorization := false
	if compiled.pathTemplate != "" {
		// Contribution routes already authenticate and authorize at Gateway;
		// preserve the sanitized caller identity but never leak the bearer.
		compiled.stripPrefix = ""
		compiled.rewritePrefix = ""
	}
	compiled.proxy = newReverseProxy(
		target,
		route.Prefix,
		compiled.stripPrefix,
		compiled.rewritePrefix,
		forwardAuthorization,
		totalTimeout,
		p.internalSigner,
		p.log,
	)
	return compiled, true
}

func (p *ServiceProxy) compileInternalRoute(route servicestatus.ServiceRoute) (routeProxy, bool) {
	apiID := strings.TrimSpace(route.ApiID)
	if apiID == "" {
		return routeProxy{}, false
	}
	target, ok := routeUpstreamBaseTarget(route)
	if !ok {
		return routeProxy{}, false
	}
	providerService := firstNonEmpty(route.ProviderService, route.ServiceID, route.TargetService)
	providerEndpoint := strings.TrimSpace(route.ProviderEndpoint)
	virtualPrefix := internalAPIPrefix + "/" + apiID
	stripPrefix := cleanPrefix(firstNonEmpty(route.StripPrefix, virtualPrefix))
	rewritePrefix := cleanPrefix(firstNonEmpty(route.RewritePrefix, route.Prefix))
	totalTimeout := routeTotalTimeout(route.TimeoutMS, 5*time.Minute)
	return routeProxy{
		prefix:               cleanPrefix(route.Prefix),
		apiID:                apiID,
		bindingID:            route.BindingID,
		consumerDeploymentID: route.ConsumerDeploymentID,
		consumerServiceID:    route.ConsumerServiceID,
		consumerNodeID:       route.ConsumerNodeID,
		credentialGeneration: route.CredentialGeneration,
		timeoutMS:            durationMilliseconds(totalTimeout),
		providerNodeID:       strings.TrimSpace(route.ProviderNodeID),
		providerService:      providerService,
		providerEndpoint:     providerEndpoint,
		serviceID:            firstNonEmpty(route.ServiceID, providerService),
		authMode:             route.AuthMode,
		providerAuthMode:     route.ProviderAuthMode,
		requiredPermission:   normalizeRequiredPermission(route.RequiredPermission),
		permissionScope:      clonePermissionScope(route.PermissionScope),
		stripPrefix:          stripPrefix,
		rewritePrefix:        rewritePrefix,
		proxy: newReverseProxy(
			target,
			virtualPrefix,
			stripPrefix,
			rewritePrefix,
			forwardServiceCallerAuthorization(apiID, providerService, firstNonEmpty(route.ProviderAuthMode, route.AuthMode)),
			totalTimeout,
			p.internalSigner,
			p.log,
		),
		target: target,
	}, true
}

func (p *ServiceProxy) acquireRouteTable() *compiledRouteTable {
	for {
		table := p.table.Load()
		if table == nil {
			return nil
		}
		if table.acquire() {
			return table
		}
	}
}

func (t *compiledRouteTable) acquire() bool {
	if t == nil {
		return false
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.retired {
		return false
	}
	t.users++
	return true
}

func (t *compiledRouteTable) release() {
	if t == nil {
		return
	}
	t.mu.Lock()
	if t.users > 0 {
		t.users--
	}
	closeNow := t.retired && t.users == 0
	t.mu.Unlock()
	if closeNow {
		t.closeTransports()
	}
}

func (t *compiledRouteTable) retire() {
	if t == nil {
		return
	}
	t.mu.Lock()
	t.retired = true
	closeNow := t.users == 0
	t.mu.Unlock()
	if closeNow {
		t.closeTransports()
	}
}

func (t *compiledRouteTable) closeTransports() {
	if t == nil {
		return
	}
	t.close.Do(func() {
		routes := make([]routeProxy, 0, len(t.routes)*2)
		for _, route := range t.routes {
			if route.serviceReady {
				routes = append(routes, route.service)
			}
			if route.internalReady {
				routes = append(routes, route.internal)
			}
		}
		closeRouteTransports(routes)
	})
}

func cloneRouteTable(table servicestatus.RouteTable) servicestatus.RouteTable {
	table.Routes = cloneServiceRoutes(table.Routes)
	table.Warnings = append([]string(nil), table.Warnings...)
	return table
}

func cloneServiceRoutes(routes []servicestatus.ServiceRoute) []servicestatus.ServiceRoute {
	cloned := make([]servicestatus.ServiceRoute, len(routes))
	for i, route := range routes {
		route.Methods = append([]string(nil), route.Methods...)
		route.Conflicts = append([]string(nil), route.Conflicts...)
		route.Warnings = append([]string(nil), route.Warnings...)
		route.BlockedBy = append([]string(nil), route.BlockedBy...)
		route.PermissionScope = clonePermissionScope(route.PermissionScope)
		cloned[i] = route
	}
	return cloned
}

func clonePermissionScope(scope *servicestatus.PermissionScope) *servicestatus.PermissionScope {
	if scope == nil {
		return nil
	}
	cloned := *scope
	return &cloned
}

// Close retires all dynamic revisions and drains the static connection pools.
// Active requests keep their snapshot until completion; no new request is
// admitted after the closed flag becomes visible.
func (p *ServiceProxy) Close() {
	if p == nil || !p.closed.CompareAndSwap(false, true) {
		return
	}
	p.tableMu.Lock()
	previous := p.table.Swap(nil)
	p.tableMu.Unlock()
	previous.retire()
	closeRouteTransports(p.staticRoutes)
}

func closeRouteTransports(routes []routeProxy) {
	seen := make(map[*http.Transport]struct{}, len(routes))
	for _, route := range routes {
		if route.proxy == nil {
			continue
		}
		transport, ok := route.proxy.Transport.(*http.Transport)
		if !ok || transport == nil {
			continue
		}
		if _, exists := seen[transport]; exists {
			continue
		}
		seen[transport] = struct{}{}
		transport.CloseIdleConnections()
	}
}

func (p *ServiceProxy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if p.closed.Load() {
		writeJSONError(w, http.StatusServiceUnavailable, 50303, "gateway proxy is shutting down")
		return
	}
	if p.extensionArtifacts != nil && p.extensionArtifacts.serve(w, r) {
		return
	}
	for _, route := range p.staticRoutes {
		if isCoreStaticProxyPrefix(route.prefix) && matchPrefix(r.URL.Path, route.prefix) {
			p.serveRoute(w, r, route)
			return
		}
	}
	snapshot := p.acquireRouteTable()
	if snapshot == nil {
		writeJSONError(w, http.StatusServiceUnavailable, 50303, "gateway proxy is shutting down")
		return
	}
	defer snapshot.release()

	if matchPrefix(r.URL.Path, internalAPIPrefix) {
		p.serveInternalAPI(w, r, snapshot)
		return
	}

	if route, ok := p.matchServiceRouteRequest(snapshot, r); ok {
		if route.pathTemplate != "" {
			params, matched := matchPathTemplate(route.pathTemplate, r.URL.Path)
			if !matched {
				http.NotFound(w, r)
				return
			}
			providerPath, rewriteOK := expandProviderPath(route.providerPath, params)
			if !rewriteOK {
				writeJSONError(w, http.StatusInternalServerError, 50005, "invalid contribution provider path")
				return
			}
			ctx := context.WithValue(r.Context(), matchedPathParametersContextKey{}, params)
			r = cloneRequestWithPath(r.WithContext(ctx), providerPath)
		}
		p.serveRoute(w, r, route)
		return
	}
	if blocked, ok := p.matchBlockedServiceRoute(snapshot, r.URL.Path); ok {
		if _, _, ok := p.authenticateRequest(w, r, blocked.authMode, blocked); !ok {
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

func (p *ServiceProxy) matchBlockedServiceRoute(snapshot *compiledRouteTable, path string) (routeProxy, bool) {
	for _, compiled := range snapshot.routes {
		route := compiled.source
		if route.ProxyEnabled || !route.Enabled || !matchPrefix(path, route.Prefix) {
			continue
		}
		if route.Status == "unavailable" || containsString(route.BlockedBy, "service not running") || containsString(route.BlockedBy, "service degraded") {
			return routeProxy{prefix: route.Prefix, serviceID: route.ServiceID, authMode: route.AuthMode}, true
		}
	}
	return routeProxy{}, false
}

func (p *ServiceProxy) matchServiceRoute(snapshot *compiledRouteTable, path string) (routeProxy, bool) {
	return p.matchServiceRouteMethod(snapshot, "", path)
}

func (p *ServiceProxy) matchServiceRouteRequest(snapshot *compiledRouteTable, request *http.Request) (routeProxy, bool) {
	if request == nil {
		return routeProxy{}, false
	}
	return p.matchServiceRouteAudience(snapshot, request.Method, request.URL.Path, requestAudience(request))
}

func (p *ServiceProxy) matchServiceRouteMethod(snapshot *compiledRouteTable, method string, path string) (routeProxy, bool) {
	return p.matchServiceRouteAudience(snapshot, method, path, "")
}

func (p *ServiceProxy) matchServiceRouteAudience(snapshot *compiledRouteTable, method string, path string, audience string) (routeProxy, bool) {
	for _, compiled := range snapshot.routes {
		if !compiled.serviceReady || !methodAllowed(method, compiled.source.Methods) {
			continue
		}
		if compiled.source.PathTemplate != "" && !audienceMatches(compiled.source.Audience, audience) {
			continue
		}
		if compiled.source.PathTemplate != "" {
			if _, ok := matchPathTemplate(compiled.source.PathTemplate, path); !ok {
				continue
			}
		} else if !matchPrefix(path, compiled.source.Prefix) {
			continue
		}
		return compiled.service, true
	}
	return routeProxy{}, false
}

func requestAudience(request *http.Request) string {
	if request == nil {
		return ""
	}
	audience := strings.ToLower(strings.TrimSpace(request.Header.Get(contributionAudienceHeader)))
	if audience != "" {
		return audience
	}
	if matchPrefix(request.URL.Path, "/api/admin") {
		return "admin"
	}
	return "user"
}

func audienceMatches(routeAudience string, requestAudience string) bool {
	routeAudience = strings.ToLower(strings.TrimSpace(routeAudience))
	requestAudience = strings.ToLower(strings.TrimSpace(requestAudience))
	if routeAudience == "public" {
		return requestAudience == "public" || requestAudience == "user"
	}
	return routeAudience == requestAudience
}

func cloneRequestWithPath(request *http.Request, path string) *http.Request {
	cloned := request.Clone(request.Context())
	urlCopy := *request.URL
	urlCopy.Path = path
	urlCopy.RawPath = ""
	cloned.URL = &urlCopy
	return cloned
}

func matchPathTemplate(template string, requestPath string) (map[string]string, bool) {
	templateSegments, ok := splitTemplatePath(template)
	if !ok {
		return nil, false
	}
	pathSegments, ok := splitEscapedRequestPath(requestPath)
	if !ok || len(pathSegments) != len(templateSegments) {
		return nil, false
	}
	params := make(map[string]string)
	for index, segment := range templateSegments {
		if name, parameter := templateParameter(segment); parameter {
			if pathSegments[index] == "" || pathSegments[index] == "." || pathSegments[index] == ".." {
				return nil, false
			}
			params[name] = pathSegments[index]
			continue
		}
		literal, err := url.PathUnescape(pathSegments[index])
		if err != nil || literal != segment {
			return nil, false
		}
	}
	return params, true
}

func expandProviderPath(template string, params map[string]string) (string, bool) {
	segments, ok := splitTemplatePath(template)
	if !ok {
		return "", false
	}
	for index, segment := range segments {
		name, parameter := templateParameter(segment)
		if !parameter {
			segments[index] = url.PathEscape(segment)
			continue
		}
		value, exists := params[name]
		if !exists || value == "" || value == "." || value == ".." || strings.ContainsAny(value, "/\\") {
			return "", false
		}
		decoded, err := url.PathUnescape(value)
		if err != nil || decoded == "" || decoded == "." || decoded == ".." || strings.ContainsAny(decoded, "/\\") {
			return "", false
		}
		segments[index] = url.PathEscape(decoded)
	}
	return "/" + strings.Join(segments, "/"), true
}

func splitTemplatePath(path string) ([]string, bool) {
	path = cleanPrefix(path)
	if path == "" || path == "/" {
		return []string{}, path == "/"
	}
	segments := strings.Split(strings.TrimPrefix(path, "/"), "/")
	seen := make(map[string]bool)
	for _, segment := range segments {
		if segment == "" || segment == "." || segment == ".." {
			return nil, false
		}
		if name, parameter := templateParameter(segment); parameter {
			if name == "" || seen[name] {
				return nil, false
			}
			seen[name] = true
		} else if strings.ContainsAny(segment, "{}") {
			return nil, false
		}
	}
	return segments, true
}

func splitEscapedRequestPath(path string) ([]string, bool) {
	if path == "/" {
		return []string{}, true
	}
	if path == "" || !strings.HasPrefix(path, "/") || strings.HasSuffix(path, "/") {
		return nil, false
	}
	segments := strings.Split(strings.TrimPrefix(path, "/"), "/")
	for _, segment := range segments {
		if segment == "" {
			return nil, false
		}
	}
	return segments, true
}

func templateParameter(segment string) (string, bool) {
	if len(segment) >= 3 && strings.HasPrefix(segment, "{") && strings.HasSuffix(segment, "}") {
		return strings.TrimSpace(segment[1 : len(segment)-1]), true
	}
	return "", false
}

func (p *ServiceProxy) serveInternalAPI(w http.ResponseWriter, r *http.Request, snapshot *compiledRouteTable) {
	apiID, _, ok := internalAPIRequest(r.URL.Path)
	if !ok {
		http.NotFound(w, r)
		return
	}
	workloadClaims, workloadErr := p.workloadClaimsFromRequest(r)
	consumerDeploymentID := ""
	callerNodeID := ""
	if workloadClaims != nil {
		consumerDeploymentID = workloadClaims.DeploymentID
		callerNodeID = workloadClaims.NodeID
	}
	if callerNodeID == "" {
		callerNodeID = strings.TrimSpace(r.Header.Get("X-OJOS-Node-Id"))
	}
	if callerNodeID == "" {
		callerNodeID = p.nodeID
	}

	route, found, unavailable, scoped := p.matchInternalAPIRouteForDeployment(snapshot, apiID, r.Method, consumerDeploymentID)
	if !found && scoped && workloadErr != nil {
		writeJSONError(w, http.StatusUnauthorized, 40107, "invalid or expired workload token")
		return
	}
	if !found && scoped && workloadClaims != nil {
		writeJSONError(w, http.StatusForbidden, 40306, "workload binding is inactive")
		return
	}
	if unavailable {
		writeJSONError(w, http.StatusServiceUnavailable, 50302, "api route not available: "+apiID)
		return
	}
	if !found {
		http.NotFound(w, r)
		return
	}
	if route.consumerDeploymentID != "" && workloadClaims == nil {
		writeJSONError(w, http.StatusUnauthorized, 40107, "workload token is required")
		return
	}
	if callerNodeID == "" {
		writeJSONError(w, http.StatusBadRequest, 40001, "caller node id is required")
		return
	}
	route.callerNodeID = callerNodeID
	r = cloneRequestWithResolverHeaders(r, route)
	if workloadClaims != nil {
		r = r.WithContext(context.WithValue(r.Context(), workloadClaimsContextKey{}, workloadClaims))
	}
	p.serveRoute(w, r, route)
}

func (p *ServiceProxy) matchInternalAPIRoute(snapshot *compiledRouteTable, apiID string, method string) (routeProxy, bool, bool) {
	route, found, unavailable, _ := p.matchInternalAPIRouteForDeployment(snapshot, apiID, method, "")
	return route, found, unavailable
}

func (p *ServiceProxy) matchInternalAPIRouteForDeployment(snapshot *compiledRouteTable, apiID string, method string, consumerDeploymentID string) (routeProxy, bool, bool, bool) {
	hasUnavailable := false
	hasScoped := false
	for _, compiled := range snapshot.routes {
		route := compiled.source
		if strings.TrimSpace(route.ApiID) != apiID {
			continue
		}
		if !methodAllowed(method, route.Methods) {
			continue
		}
		routeConsumer := strings.TrimSpace(route.ConsumerDeploymentID)
		if routeConsumer != "" {
			hasScoped = true
			if consumerDeploymentID == "" || routeConsumer != consumerDeploymentID {
				continue
			}
		} else if consumerDeploymentID != "" {
			// Workload identities never fall back to an unscoped provider route.
			continue
		}
		if !route.ProxyEnabled || !compiled.internalReady {
			hasUnavailable = true
			continue
		}
		return compiled.internal, true, false, hasScoped
	}
	return routeProxy{}, false, hasUnavailable, hasScoped
}

func (p *ServiceProxy) workloadClaimsFromRequest(r *http.Request) (*workload.Claims, error) {
	if p.workloadVerifier == nil || r == nil {
		return nil, fmt.Errorf("workload verifier is unavailable")
	}
	parts := strings.Fields(strings.TrimSpace(r.Header.Get("Authorization")))
	if len(parts) != 2 || !strings.EqualFold(parts[0], "Bearer") {
		return nil, fmt.Errorf("workload bearer token is missing")
	}
	return p.workloadVerifier.Verify(strings.TrimSpace(parts[1]), time.Now())
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
	if route.timeoutMS > 0 {
		ctx, cancel := context.WithTimeout(r.Context(), time.Duration(route.timeoutMS)*time.Millisecond)
		defer cancel()
		r = r.WithContext(ctx)
	}

	authMode := route.authMode
	if route.requiredPermission != "" && normalizeServiceAuthMode(authMode) == authModePublic {
		authMode = authModeUser
	}
	if normalizeServiceAuthMode(authMode) == authModeService &&
		normalizeRequiredPermission(route.requiredPermission) == "" {
		writeJSONError(w, http.StatusInternalServerError, 50004, "service route requires a non-public permission")
		return
	}
	caller, claims, ok := p.authenticateRequest(w, r, authMode, route)
	if !ok {
		return
	}

	if !p.authorizeRequiredPermission(w, r, caller, route.requiredPermission, route.permissionScope) {
		return
	}

	if claims != nil {
		ctx := context.WithValue(r.Context(), claimsContextKey{}, claims)
		r = r.WithContext(ctx)
	}
	if caller.Type == authModeWorkload {
		workloadClaims, _ := r.Context().Value(workloadClaimsContextKey{}).(*workload.Claims)
		if workloadClaims == nil {
			workloadClaims, _ = p.workloadClaimsFromRequest(r)
		}
		if workloadClaims != nil {
			cloned := r.Clone(r.Context())
			cloned.Header = r.Header.Clone()
			for _, header := range []string{
				"X-OJOS-Caller-Service",
				"X-OJOS-Node-Id",
				"X-OJOS-Caller-Node-Id",
				"X-OJOS-Caller-Deployment-Id",
				"X-OJOS-Binding-Id",
			} {
				cloned.Header.Del(header)
			}
			cloned.Header.Set("X-OJOS-Caller-Service", workloadClaims.ServiceID)
			cloned.Header.Set("X-OJOS-Node-Id", workloadClaims.NodeID)
			cloned.Header.Set("X-OJOS-Caller-Node-Id", workloadClaims.NodeID)
			cloned.Header.Set("X-OJOS-Caller-Deployment-Id", workloadClaims.DeploymentID)
			cloned.Header.Set("X-OJOS-Binding-Id", route.bindingID)
			r = cloned
		}
	}

	route.proxy.ServeHTTP(w, r)
}

func (p *ServiceProxy) authorizeRequiredPermission(
	w http.ResponseWriter,
	r *http.Request,
	caller PermissionCheckCaller,
	requiredPermission string,
	permissionScope *servicestatus.PermissionScope,
) bool {
	requiredPermission = normalizeRequiredPermission(requiredPermission)
	if requiredPermission == "" {
		return true
	}
	if caller.Type == authModeWorkload {
		// An exact active ApiBinding is the workload grant. The binding resolver
		// already matched the required permission when it published this route.
		return true
	}
	if caller.Type == "" {
		writeJSONError(w, http.StatusUnauthorized, 40105, "missing authorization claims")
		return false
	}
	if p.permissionChecker == nil {
		writeJSONError(w, http.StatusInternalServerError, 50002, "permission check unavailable")
		return false
	}
	scopeType, scopeID, validScope := resolvePermissionScope(r, permissionScope)
	if !validScope {
		writeJSONError(w, http.StatusBadRequest, 40005, "invalid permission scope id")
		return false
	}
	caller.ScopeType = scopeType
	caller.ScopeID = scopeID
	ok, err := p.permissionChecker(
		r.Context(),
		strings.TrimSpace(r.Header.Get("Authorization")),
		caller,
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

func resolvePermissionScope(r *http.Request, scope *servicestatus.PermissionScope) (string, int64, bool) {
	if scope == nil || scope.Kind == "system" {
		return "system", 0, true
	}
	if scope.Kind != "path_parameter" || strings.TrimSpace(scope.Type) == "" || strings.TrimSpace(scope.PathParameter) == "" {
		return "", 0, false
	}
	params, _ := r.Context().Value(matchedPathParametersContextKey{}).(map[string]string)
	raw, exists := params[scope.PathParameter]
	if !exists {
		return "", 0, false
	}
	decoded, err := url.PathUnescape(raw)
	if err != nil || decoded == "" || strings.TrimSpace(decoded) != decoded || strings.HasPrefix(decoded, "+") {
		return "", 0, false
	}
	id, err := strconv.ParseInt(decoded, 10, 64)
	if err != nil || id <= 0 || strconv.FormatInt(id, 10) != decoded {
		return "", 0, false
	}
	return scope.Type, id, true
}

func (p *ServiceProxy) authenticateRequest(
	w http.ResponseWriter,
	r *http.Request,
	authMode string,
	route routeProxy,
) (PermissionCheckCaller, *sharedjwt.Claims, bool) {
	authMode = normalizeServiceAuthMode(authMode)
	if authMode == authModePublic {
		return PermissionCheckCaller{}, nil, true
	}
	if authMode == authModeInternal {
		writeJSONError(w, http.StatusForbidden, 40303, "internal route is not public")
		return PermissionCheckCaller{}, nil, false
	}
	if authMode == authModeWorker {
		writeJSONError(w, http.StatusForbidden, 40304, "worker route is not available through dynamic proxy")
		return PermissionCheckCaller{}, nil, false
	}
	if authMode == authModeWorkload {
		claims, _ := r.Context().Value(workloadClaimsContextKey{}).(*workload.Claims)
		if claims == nil {
			var err error
			claims, err = p.workloadClaimsFromRequest(r)
			if err != nil {
				writeJSONError(w, http.StatusUnauthorized, 40107, "invalid or expired workload token")
				return PermissionCheckCaller{}, nil, false
			}
		}
		if route.bindingID == "" || route.consumerDeploymentID == "" ||
			claims.DeploymentID != route.consumerDeploymentID ||
			(route.consumerServiceID != "" && claims.ServiceID != route.consumerServiceID) ||
			(route.consumerNodeID != "" && claims.NodeID != route.consumerNodeID) ||
			(route.credentialGeneration > 0 && claims.CredentialGeneration != route.credentialGeneration) {
			writeJSONError(w, http.StatusForbidden, 40306, "workload binding is inactive")
			return PermissionCheckCaller{}, nil, false
		}
		return PermissionCheckCaller{
			Type:    authModeWorkload,
			Service: claims.ServiceID,
			NodeID:  claims.NodeID,
			APIID:   route.apiID,
		}, nil, true
	}

	authHeader := strings.TrimSpace(r.Header.Get("Authorization"))
	if authHeader == "" {
		if authMode == authModeOptional {
			return PermissionCheckCaller{}, nil, true
		}

		writeJSONError(w, http.StatusUnauthorized, 40101, "missing authorization header")
		return PermissionCheckCaller{}, nil, false
	}

	if authMode == authModeService {
		callerService := strings.TrimSpace(r.Header.Get("X-OJOS-Caller-Service"))
		if callerService == "" {
			writeJSONError(w, http.StatusUnauthorized, 40106, "caller service is required")
			return PermissionCheckCaller{}, nil, false
		}
		return PermissionCheckCaller{
			Type:    authModeService,
			Service: callerService,
			NodeID:  strings.TrimSpace(r.Header.Get("X-OJOS-Node-Id")),
			APIID:   route.apiID,
		}, nil, true
	}

	parts := strings.Fields(authHeader)
	if len(parts) != 2 || !strings.EqualFold(parts[0], "Bearer") {
		writeJSONError(w, http.StatusUnauthorized, 40102, "invalid authorization header")
		return PermissionCheckCaller{}, nil, false
	}

	tokenString := strings.TrimSpace(parts[1])
	if tokenString == "" {
		writeJSONError(w, http.StatusUnauthorized, 40103, "empty token")
		return PermissionCheckCaller{}, nil, false
	}

	claims, err := sharedjwt.Parse(p.jwtSecret, tokenString)
	if err != nil {
		writeJSONError(w, http.StatusUnauthorized, 40104, "invalid or expired token")
		return PermissionCheckCaller{}, nil, false
	}
	caller := PermissionCheckCaller{
		Type:   authModeUser,
		UserID: claims.UserID,
		NodeID: strings.TrimSpace(r.Header.Get("X-OJOS-Node-Id")),
		APIID:  route.apiID,
	}

	if authMode == authModeAdmin {
		if isAdminRole(claims.Roles) {
			caller.Type = authModeAdmin
			return caller, claims, true
		}
		if p.adminChecker != nil {
			ok, err := p.adminChecker(
				r.Context(),
				strings.TrimSpace(r.Header.Get("Authorization")),
				claims.UserID,
			)
			if err != nil {
				writeJSONError(w, http.StatusInternalServerError, 50001, "authorization check failed")
				return PermissionCheckCaller{}, nil, false
			}
			if ok {
				caller.Type = authModeAdmin
				return caller, claims, true
			}
		}
		writeJSONError(w, http.StatusForbidden, 40301, "forbidden")
		return PermissionCheckCaller{}, nil, false
	}

	return caller, claims, true
}

func claimsFromContext(ctx context.Context) (*sharedjwt.Claims, bool) {
	claims, ok := ctx.Value(claimsContextKey{}).(*sharedjwt.Claims)
	return claims, ok
}

func normalizeAuthMode(mode string) (string, error) {
	mode = normalizeServiceAuthMode(mode)
	switch mode {
	case authModePublic, authModeOptional, authModeUser, authModeAdmin, authModeWorker, authModeInternal, authModeService, authModeWorkload:
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
	case "service":
		return authModeService
	case "workload":
		return authModeWorkload
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
	if method == "" {
		return true
	}
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
	responseHeaderTimeout time.Duration,
	internalSigner *internalauth.Signer,
	log *zap.Logger,
) *httputil.ReverseProxy {
	return &httputil.ReverseProxy{
		// A negative interval flushes every write. In particular, this avoids
		// accumulating object downloads or long-poll responses in the Gateway.
		FlushInterval: -1,
		Rewrite: func(pr *httputil.ProxyRequest) {
			originalPath := pr.In.URL.Path
			originalQuery := pr.In.URL.RawQuery

			upstreamPath := originalPath
			if stripPrefix != "" {
				upstreamPath = strings.TrimPrefix(upstreamPath, stripPrefix)
			}
			if rewritePrefix != "" {
				// An api_id call with no trailing segments (a plain POST carrying
				// a JSON body) leaves nothing to join, and joining would append a
				// stray trailing slash to the upstream path.
				if upstreamPath == "" || upstreamPath == "/" {
					upstreamPath = rewritePrefix
				} else {
					upstreamPath = singleJoiningSlash(rewritePrefix, upstreamPath)
				}
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
			pr.Out.Header.Del(contributionAudienceHeader)

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
			ResponseHeaderTimeout: responseHeaderTimeout,
			MaxIdleConns:          256,
			MaxIdleConnsPerHost:   32,
			IdleConnTimeout:       90 * time.Second,
			TLSHandshakeTimeout:   10 * time.Second,
			ExpectContinueTimeout: time.Second,
		},
	}
}

// Only auth-service needs the original service credential so it can verify the
// caller a second time before answering a delegated user-permission query.
// Forwarding the bearer to unrelated providers would expose a reusable secret.
func forwardServiceCallerAuthorization(apiID string, providerService string, authMode string) bool {
	if normalizeServiceAuthMode(authMode) == authModeWorkload {
		return true
	}
	return strings.TrimSpace(apiID) == "auth.user.permission.check" &&
		strings.TrimSpace(providerService) == "auth-service" &&
		normalizeServiceAuthMode(authMode) == authModeService
}

func routeTotalTimeout(timeoutMS uint64, fallback time.Duration) time.Duration {
	if timeoutMS == 0 {
		timeoutMS = durationMilliseconds(fallback)
	}
	const minTimeoutMS = uint64(time.Second / time.Millisecond)
	const maxTimeoutMS = uint64((10 * time.Minute) / time.Millisecond)
	if timeoutMS < minTimeoutMS {
		timeoutMS = minTimeoutMS
	}
	if timeoutMS > maxTimeoutMS {
		timeoutMS = maxTimeoutMS
	}
	return time.Duration(timeoutMS) * time.Millisecond
}

func durationMilliseconds(value time.Duration) uint64 {
	if value <= 0 {
		return 0
	}
	return uint64(value / time.Millisecond)
}

func staticRouteFallbackTimeout(prefix string) time.Duration {
	if cleanPrefix(prefix) == "/api/judge/worker" {
		return 35 * time.Second
	}
	return 30 * time.Second
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
