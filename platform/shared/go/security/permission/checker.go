package permission

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	"ojos-shared/servicecontext"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/zeromicro/go-zero/core/logx"
)

type UserChecker interface {
	RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) error
	HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) (bool, error)
}

type DatabaseUserChecker struct {
	DB *pgxpool.Pool
}

func NewDatabaseUserChecker(db *pgxpool.Pool) UserChecker {
	if db == nil {
		return nil
	}
	return DatabaseUserChecker{DB: db}
}

func (p DatabaseUserChecker) RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) error {
	if p.DB == nil {
		return errors.New("permission database is not configured")
	}
	return RequireUserPermission(ctx, p.DB, userID, permissionCode, scope)
}

func (p DatabaseUserChecker) HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) (bool, error) {
	if p.DB == nil {
		return false, errors.New("permission database is not configured")
	}
	return HasUserPermission(ctx, p.DB, userID, permissionCode, scope)
}

const (
	// DefaultPermissionCheckApiID is the API surface auth-service declares in its
	// release.yaml for "does user U hold permission P inside scope S". Callers
	// never learn auth-service's address: the orchestrator resolves this api_id
	// into an effective route and the gateway performs the forwarding.
	DefaultPermissionCheckApiID = "auth.user.permission.check"

	// directPermissionCheckPath is the auth-service local path behind
	// DefaultPermissionCheckApiID. It is only used on the fallback route, where
	// the caller reaches auth-service directly.
	directPermissionCheckPath = "/auth/admin/permission-check"

	// internalAPIPathPrefix is the gateway entry point for orchestrator-resolved
	// API surfaces. Same shape as the storage-service clients.
	internalAPIPathPrefix = "/internal/apis/"

	// RouteInternalGateway is the preferred route: internal gateway + api_id.
	RouteInternalGateway = "internal-gateway"

	// RouteAuthService is the fallback route: a directly configured auth-service
	// address used while the gateway route is not configured completely.
	RouteAuthService = "auth-service"

	// RouteServiceContext is the production Service Contract v2 route. The
	// concrete provider and the short-lived credential are supplied by the
	// Agent-materialized context, not application configuration.
	RouteServiceContext = "service-context"

	DefaultPermissionCheckBinding = "permission_check"
)

// RemoteCheckerConfig describes how a service reaches the permission check API.
//
// The preferred route is InternalGatewayEndpoint + ApiID: the service declares
// nothing about auth-service's location, the orchestrator computes the effective
// route, and the gateway forwards the call. AuthServiceEndpoint remains as a
// fallback while the gateway route is not configured completely. Route selection
// happens once at construction; a later gateway failure stays fail-closed.
type RemoteCheckerConfig struct {
	InternalGatewayEndpoint string
	ApiID                   string
	CallerService           string
	CallerNodeID            string
	ServiceToken            string

	AuthServiceEndpoint   string
	AuthServiceAdminToken string
}

// RemoteUserChecker performs the permission check over HTTP. The route is fixed
// at construction time and named in every log line and error message, so an
// operator can always tell which path a check took.
type RemoteUserChecker struct {
	route         string
	url           string
	apiID         string
	callerService string
	callerNodeID  string
	token         string
	client        *http.Client
}

// ServiceContextUserChecker addresses the permission API by requirement name.
// ServiceContext.NewRequest reloads the workload credential for every request,
// so token rotation never requires restarting the service.
type ServiceContextUserChecker struct {
	context     servicecontext.ServiceContext
	bindingName string
	client      *http.Client
}

type permissionCheckRequest struct {
	UserID        int64  `json:"user_id,omitempty"`
	Permission    string `json:"permission"`
	ScopeType     string `json:"scope_type,omitempty"`
	ScopeID       int64  `json:"scope_id,omitempty"`
	CallerService string `json:"caller_service,omitempty"`
	CallerNodeID  string `json:"caller_node_id,omitempty"`
	ApiID         string `json:"api_id,omitempty"`
}

type permissionCheckResponse struct {
	Code int    `json:"code"`
	Msg  string `json:"msg"`
	Data struct {
		Allowed bool `json:"allowed"`
	} `json:"data"`
}

// NewRemoteUserChecker resolves the route once. It returns nil when neither the
// gateway route nor the direct route is usable, so callers can fall back to the
// local database checker.
func NewRemoteUserChecker(cfg RemoteCheckerConfig) UserChecker {
	apiID := strings.TrimSpace(cfg.ApiID)
	if apiID == "" {
		apiID = DefaultPermissionCheckApiID
	}
	callerService := strings.TrimSpace(cfg.CallerService)
	callerNodeID := strings.TrimSpace(cfg.CallerNodeID)

	gateway := strings.TrimRight(strings.TrimSpace(cfg.InternalGatewayEndpoint), "/")
	serviceToken := strings.TrimSpace(cfg.ServiceToken)

	if gateway != "" {
		if serviceToken == "" || callerService == "" {
			logx.Errorf(
				"permission check gateway route is incomplete (service_token_set=%t caller_service=%q); falling back to route=%s",
				serviceToken != "",
				callerService,
				RouteAuthService,
			)
		} else {
			logx.Infof(
				"permission check route=%s gateway=%s api_id=%s caller_service=%s caller_node_id=%q",
				RouteInternalGateway,
				gateway,
				apiID,
				callerService,
				callerNodeID,
			)
			return RemoteUserChecker{
				route:         RouteInternalGateway,
				url:           gateway + internalAPIPathPrefix + url.PathEscape(apiID),
				apiID:         apiID,
				callerService: callerService,
				callerNodeID:  callerNodeID,
				token:         serviceToken,
				client:        &http.Client{Timeout: 5 * time.Second},
			}
		}
	}

	endpoint := strings.TrimRight(strings.TrimSpace(cfg.AuthServiceEndpoint), "/")
	adminToken := strings.TrimSpace(cfg.AuthServiceAdminToken)
	if endpoint == "" || adminToken == "" {
		return nil
	}
	logx.Infof(
		"permission check route=%s endpoint=%s path=%s api_id=%s",
		RouteAuthService,
		endpoint,
		directPermissionCheckPath,
		apiID,
	)
	return RemoteUserChecker{
		route:         RouteAuthService,
		url:           endpoint + directPermissionCheckPath,
		apiID:         apiID,
		callerService: callerService,
		callerNodeID:  callerNodeID,
		token:         adminToken,
		client:        &http.Client{Timeout: 5 * time.Second},
	}
}

// NewAuthServiceUserChecker builds a checker pinned to the direct auth-service
// route, for callers that only hold an address and a token.
func NewAuthServiceUserChecker(endpoint string, adminToken string) UserChecker {
	return NewRemoteUserChecker(RemoteCheckerConfig{
		AuthServiceEndpoint:   endpoint,
		AuthServiceAdminToken: adminToken,
	})
}

// NewServiceContextUserChecker constructs the fail-closed managed checker. A
// missing or incorrectly bound requirement is a configuration error rather
// than a reason to fall back to a database or a global management token.
func NewServiceContextUserChecker(value *servicecontext.ServiceContext, bindingName string) (UserChecker, error) {
	if value == nil {
		return nil, errors.New("managed service context is required")
	}
	bindingName = strings.TrimSpace(bindingName)
	if bindingName == "" {
		bindingName = DefaultPermissionCheckBinding
	}
	binding, err := value.Binding(bindingName)
	if err != nil {
		return nil, err
	}
	if binding.APIID != DefaultPermissionCheckApiID {
		return nil, fmt.Errorf("binding %q resolves %s, expected %s", bindingName, binding.APIID, DefaultPermissionCheckApiID)
	}
	client, err := value.Client()
	if err != nil {
		return nil, fmt.Errorf("configure permission binding client: %w", err)
	}
	return ServiceContextUserChecker{context: *value, bindingName: bindingName, client: client}, nil
}

// NewManagedOrLegacyUserChecker is the service startup boundary. Managed
// workloads must use the named ApiBinding; legacy URL/token/database routing is
// retained only when no Service Context is present (local Compose/development).
func NewManagedOrLegacyUserChecker(expectedService, bindingName string, cfg RemoteCheckerConfig, db *pgxpool.Pool) (UserChecker, error) {
	value, err := servicecontext.LoadOptional()
	if err != nil {
		return nil, err
	}
	if value == nil {
		if strings.EqualFold(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")), "production") {
			return nil, errors.New("production permission checks require a managed Service Context")
		}
		return NewUserCheckerWithConfig(cfg, db), nil
	}
	if err := value.RequireService(strings.TrimSpace(expectedService)); err != nil {
		return nil, err
	}
	return NewServiceContextUserChecker(value, bindingName)
}

// NewUserCheckerWithConfig prefers the orchestrator-resolved gateway route, then
// the direct auth-service route, then the local database.
func NewUserCheckerWithConfig(cfg RemoteCheckerConfig, db *pgxpool.Pool) UserChecker {
	if checker := NewRemoteUserChecker(cfg); checker != nil {
		return checker
	}
	return NewDatabaseUserChecker(db)
}

func NewUserChecker(authServiceEndpoint string, authServiceAdminToken string, db *pgxpool.Pool) UserChecker {
	return NewUserCheckerWithConfig(RemoteCheckerConfig{
		AuthServiceEndpoint:   authServiceEndpoint,
		AuthServiceAdminToken: authServiceAdminToken,
	}, db)
}

// Route reports which path this checker resolved to, for diagnostics.
func (p RemoteUserChecker) Route() string {
	return p.route
}

func (p ServiceContextUserChecker) Route() string { return RouteServiceContext }

func (p RemoteUserChecker) RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) error {
	allowed, err := p.HasUserPermission(ctx, userID, permissionCode, scope)
	if err != nil {
		return err
	}
	if !allowed {
		return ErrForbidden
	}
	return nil
}

func (p ServiceContextUserChecker) RequireUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) error {
	allowed, err := p.HasUserPermission(ctx, userID, permissionCode, scope)
	if err != nil {
		return err
	}
	if !allowed {
		return ErrForbidden
	}
	return nil
}

func (p ServiceContextUserChecker) HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) (bool, error) {
	permissionCode = strings.TrimSpace(permissionCode)
	if userID <= 0 {
		return false, errors.New("invalid user id")
	}
	if permissionCode == "" {
		return false, errors.New("permission code is empty")
	}
	scope = normalizeScope(scope)
	body, err := json.Marshal(permissionCheckRequest{
		UserID: userID, Permission: permissionCode, ScopeType: scope.Type, ScopeID: scope.ID,
	})
	if err != nil {
		return false, err
	}
	response, err := p.context.DoWithOptions(
		ctx,
		p.client,
		p.bindingName,
		http.MethodPost,
		"",
		bytes.NewReader(body),
		servicecontext.RequestOptions{
			Headers:       http.Header{"Content-Type": []string{"application/json"}},
			ContentLength: int64(len(body)),
		},
	)
	if err != nil {
		return false, fmt.Errorf("permission check via %s failed: %w", RouteServiceContext, err)
	}
	defer response.Body.Close()
	return decodePermissionResponse(response, RouteServiceContext)
}

func (p RemoteUserChecker) HasUserPermission(ctx context.Context, userID int64, permissionCode string, scope Scope) (bool, error) {
	permissionCode = strings.TrimSpace(permissionCode)
	if userID <= 0 {
		return false, errors.New("invalid user id")
	}
	if permissionCode == "" {
		return false, errors.New("permission code is empty")
	}

	scope = normalizeScope(scope)
	payload := permissionCheckRequest{
		UserID:     userID,
		Permission: permissionCode,
		ScopeType:  scope.Type,
		ScopeID:    scope.ID,
	}
	if p.route == RouteInternalGateway {
		// Audit metadata only. Authorization of the call itself is decided by the
		// gateway from the effective route's auth_mode and permission.
		payload.CallerService = p.callerService
		payload.CallerNodeID = p.callerNodeID
		payload.ApiID = p.apiID
	}

	body, err := json.Marshal(payload)
	if err != nil {
		return false, err
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, p.url, bytes.NewReader(body))
	if err != nil {
		return false, err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bearer "+p.token)
	p.addInternalGatewayHeaders(req)

	resp, err := p.client.Do(req)
	if err != nil {
		return false, fmt.Errorf("permission check via %s failed: %w", p.route, err)
	}
	defer resp.Body.Close()

	return decodePermissionResponse(resp, p.route)
}

func decodePermissionResponse(resp *http.Response, route string) (bool, error) {
	if resp.StatusCode == http.StatusUnauthorized {
		return false, fmt.Errorf("permission check via %s unauthorized", route)
	}
	if resp.StatusCode == http.StatusForbidden {
		return false, ErrForbidden
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return false, fmt.Errorf("permission check via %s returned %s", route, resp.Status)
	}
	var decoded permissionCheckResponse
	if err := json.NewDecoder(resp.Body).Decode(&decoded); err != nil {
		return false, err
	}
	if decoded.Code != 0 {
		if strings.TrimSpace(decoded.Msg) == "" {
			decoded.Msg = "permission check failed"
		}
		return false, fmt.Errorf("permission check via %s: %s", route, decoded.Msg)
	}
	return decoded.Data.Allowed, nil
}

// addInternalGatewayHeaders mirrors the storage-service clients: the gateway
// needs the caller identity to authenticate an auth_mode=service route, and the
// caller node id to select the effective route inside the node tree.
func (p RemoteUserChecker) addInternalGatewayHeaders(req *http.Request) {
	if p.route != RouteInternalGateway {
		return
	}
	if p.callerService != "" {
		req.Header.Set("X-OJOS-Caller-Service", p.callerService)
	}
	if p.callerNodeID != "" {
		req.Header.Set("X-OJOS-Node-Id", p.callerNodeID)
		req.Header.Set("X-OJOS-Caller-Node-Id", p.callerNodeID)
	}
}
