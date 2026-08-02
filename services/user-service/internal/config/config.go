// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
	rest.RestConf

	Database    DatabaseConfig
	Storage     StorageConfig
	AuthService AuthServiceConfig
}

type DatabaseConfig struct {
	Url string
}

type StorageConfig struct {
	ProfilesRoot string
}

// AuthServiceConfig configures the permission check call.
//
// Preferred route: InternalGatewayEndpoint + PermissionCheckApiID. The service
// declares no auth-service address; the orchestrator resolves the api_id into an
// effective route and the gateway forwards the request.
//
// Fallback route: Endpoint + AdminToken talk to auth-service directly. It is
// selected only while InternalGatewayEndpoint is empty or incomplete; runtime
// gateway failures stay fail-closed instead of switching trust paths.
type AuthServiceConfig struct {
	Endpoint   string `json:",optional"`
	AdminToken string `json:",optional"`

	InternalGatewayEndpoint string `json:",optional"`
	PermissionCheckApiID    string `json:",optional"`
	CallerService           string `json:",optional"`
	CallerNodeID            string `json:",optional"`
	ServiceToken            string `json:",optional"`
}
