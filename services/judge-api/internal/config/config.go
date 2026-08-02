// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
	rest.RestConf

	Database     DatabaseConfig
	Redis        RedisConfig
	Jaeger       JaegerConfig
	Storage      StorageConfig
	AuthService  AuthServiceConfig
	Submission   SubmissionConfig
	Languages    LanguagesConfig
	WorkerAuth   WorkerAuthConfig
	InternalAuth InternalAuthConfig
}

type DatabaseConfig struct {
	Url string
}

type RedisConfig struct {
	Url string
}

type JaegerConfig struct {
	Endpoint string
}

// AuthServiceConfig configures the permission check call.
//
// Preferred route: InternalGatewayEndpoint + PermissionCheckApiID. The service
// declares no auth-service address; the orchestrator resolves the api_id into an
// effective route and the gateway forwards the request, exactly like the storage
// client above.
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

type StorageConfig struct {
	SubmissionsRoot         string
	ServiceEndpoint         string `json:",optional"`
	InternalGatewayEndpoint string `json:",optional"`
	GetApiID                string `json:",optional"`
	PutApiID                string `json:",optional"`
	HeadApiID               string `json:",optional"`
	Bucket                  string `json:",optional"`
	CallerService           string `json:",optional"`
	CallerNodeID            string `json:",optional"`
	ServiceToken            string `json:",optional"`
}

type SubmissionConfig struct {
	MaxCodeBytes int64 `json:",optional"`
}

type LanguagesConfig struct {
	Items []LanguageConfig `json:",optional"`
}

type LanguageConfig struct {
	Id          string
	DisplayName string
	Version     string `json:",optional"`
	Enabled     bool
	SourceFile  string `json:",optional"`
}

type WorkerAuthConfig struct {
	Token           string `json:",optional"`
	LeaseTTLSeconds int64  `json:",optional"`
}

type InternalAuthConfig struct {
	Enabled              bool
	TimestampSkewSeconds int64 `json:",optional"`
	NonceTTLSeconds      int64 `json:",optional"`
}
