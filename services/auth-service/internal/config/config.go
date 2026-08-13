// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package config

import "github.com/zeromicro/go-zero/rest"

type Config struct {
	rest.RestConf

	Database         DatabaseConfig
	Jaeger           JaegerConfig
	Jwt              JwtConfig
	InternalAuth     InternalAuthConfig
	AdminBootstrap   AdminBootstrapConfig   `json:",optional"`
	WorkloadIdentity WorkloadIdentityConfig `json:",optional"`
	Orchestrator     OrchestratorConfig     `json:",optional"`
}

type DatabaseConfig struct {
	Url string
}

type JaegerConfig struct {
	Endpoint string
}

type JwtConfig struct {
	Secret      string
	ExpireHours int
}

type InternalAuthConfig struct {
	Token string `json:",optional"`
}

// AdminBootstrapConfig is deliberately separate from Jwt and InternalAuth.
// The bootstrap secret authorizes exactly one database-backed creation of the
// initial super administrator; it is never accepted as an API bearer token.
type AdminBootstrapConfig struct {
	Secret     string `json:",optional"`
	SecretFile string `json:",optional"`
}

type WorkloadIdentityConfig struct {
	PrivateKeyFile    string `json:",optional"`
	PrivateKeyPEM     string `json:",optional"`
	ControlPlaneToken string `json:",optional"`
	KeyID             string `json:",optional"`
	Issuer            string `json:",optional"`
	Audience          string `json:",optional"`
	TTLSeconds        int64  `json:",optional"`
}

type OrchestratorConfig struct {
	Endpoint             string `json:",optional"`
	InternalToken        string `json:",optional"`
	ManagementToken      string `json:",optional"`
	ContributionAckToken string `json:",optional"`
}
