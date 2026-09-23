package svc

import (
	"context"
	"crypto/ed25519"
	"crypto/x509"
	"encoding/pem"
	"errors"
	"fmt"
	"strings"
	"time"

	"ojos-auth-service/internal/config"
	authmw "ojos-auth-service/internal/middleware"
	atopology "ojos-auth-service/internal/topologyprojection"

	"ojos-shared/security/workload"
)

func parseWorkloadPrivateKeyPEM(value string) (ed25519.PrivateKey, error) {
	block, _ := pem.Decode([]byte(strings.TrimSpace(value)))
	if block == nil {
		return nil, errors.New("workload private key is not PEM")
	}
	parsed, err := x509.ParsePKCS8PrivateKey(block.Bytes)
	if err != nil {
		return nil, errors.New("parse workload private key")
	}
	key, ok := parsed.(ed25519.PrivateKey)
	if !ok {
		return nil, errors.New("workload private key is not Ed25519")
	}
	return key, nil
}

func newServiceRouteAuthorizer(
	production bool,
	verifier *workload.Verifier,
	projection *atopology.Store,
	legacy authmw.ServiceRouteAuthorizer,
) authmw.ServiceRouteAuthorizer {
	return func(
		ctx context.Context,
		serviceCode string,
		credentialToken string,
		apiID string,
		permissionCode string,
	) (bool, error) {
		if verifier != nil && projection != nil {
			claims, err := verifier.Verify(credentialToken, time.Now())
			if err == nil {
				if strings.TrimSpace(serviceCode) != claims.ServiceID {
					return false, nil
				}
				return projection.AuthorizeWorkload(
					ctx,
					claims.DeploymentID,
					claims.ServiceID,
					claims.NodeID,
					claims.CredentialGeneration,
					apiID,
					permissionCode,
				)
			}
		}
		if production || legacy == nil {
			return false, nil
		}
		return legacy(ctx, serviceCode, credentialToken, apiID, permissionCode)
	}
}

func validateWorkloadIdentityConfig(c config.WorkloadIdentityConfig, production bool) error {
	keyConfigured := strings.TrimSpace(c.PrivateKeyFile) != "" || strings.TrimSpace(c.PrivateKeyPEM) != ""
	if strings.TrimSpace(c.PrivateKeyFile) != "" && strings.TrimSpace(c.PrivateKeyPEM) != "" {
		return fmt.Errorf("private key file and inline PEM are mutually exclusive")
	}
	controlPlaneConfigured := strings.TrimSpace(c.ControlPlaneToken) != ""
	if keyConfigured != controlPlaneConfigured {
		return fmt.Errorf("private key and dedicated control-plane token must be configured together")
	}
	if production && !keyConfigured {
		return fmt.Errorf("production Auth requires a private key and dedicated control-plane token")
	}
	expectedTTL := int64(workload.DefaultTTL / time.Second)
	if production && c.TTLSeconds != expectedTTL {
		return fmt.Errorf("production workload identity TTL must be %d seconds", expectedTTL)
	}
	return nil
}
