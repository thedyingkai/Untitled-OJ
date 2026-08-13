// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

const minimumProxyNetworkTimeoutMS int64 = 600000

type Config struct {
	rest.RestConf

	Database         DatabaseConfig
	Redis            RedisConfig
	Jaeger           JaegerConfig
	Jwt              JwtConfig
	Storage          StorageConfig
	Proxy            ProxyConfig
	ServiceStatus    ServiceStatusConfig
	InternalAuth     InternalAuthConfig
	Orchestrator     OrchestratorConfig
	AuthService      AuthServiceConfig
	WorkloadIdentity WorkloadIdentityConfig `json:",optional"`
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

type JwtConfig struct {
	Secret string
}

type StorageConfig struct {
	ProblemsRoot    string `json:",optional"`
	SubmissionsRoot string `json:",optional"`
}

type ProxyConfig struct {
	Routes          []ProxyRouteConfig
	TrustedServices []ProxyTrustedServiceConfig `json:",optional"`
}

type ProxyRouteConfig struct {
	Prefix      string
	Target      string
	StripPrefix string `json:",optional"`
	AuthMode    string `json:",optional"`
	TimeoutMS   uint64 `json:",optional"`
}

type ProxyTrustedServiceConfig struct {
	ServiceID     string
	Target        string
	StripPrefix   string `json:",optional"`
	RewritePrefix string `json:",optional"`
	HealthCheckID string `json:",optional"`
}

type ServiceStatusConfig struct {
	ComposeServices []string `json:",optional"`
}

type InternalAuthConfig struct {
	Enabled                 bool
	RotationIntervalSeconds int64 `json:",optional"`
	VerifyGraceSeconds      int64 `json:",optional"`
	RotateBeforeSeconds     int64 `json:",optional"`
	TimestampSkewSeconds    int64 `json:",optional"`
	NonceTTLSeconds         int64 `json:",optional"`
}

type OrchestratorConfig struct {
	Endpoint             string
	InternalToken        string
	ManagementToken      string `json:",optional"`
	ContributionAckToken string `json:",optional"`
	NodeID               string `json:",optional"`
}

type AuthServiceConfig struct {
	Endpoint string
}

type WorkloadIdentityConfig struct {
	PublicKeyFile string `json:",optional"`
	KeyID         string `json:",optional"`
	Issuer        string `json:",optional"`
	Audience      string `json:",optional"`
}

// PrepareProxyServer keeps the HTTP server's socket deadline above every
// published binding while avoiding go-zero's response-buffering timeout
// middleware. ServiceProxy applies the narrower per-route total deadlines.
func (c *Config) PrepareProxyServer() {
	if c.Timeout < minimumProxyNetworkTimeoutMS {
		c.Timeout = minimumProxyNetworkTimeoutMS
	}
	c.Middlewares.Timeout = false
	c.Middlewares.Recover = false
}
