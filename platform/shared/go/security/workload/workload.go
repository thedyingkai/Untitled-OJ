package workload

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/x509"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	jwt "github.com/golang-jwt/jwt/v5"
)

const (
	DefaultIssuer   = "ojos-auth/workload"
	DefaultAudience = "ojos-gateway"
	DefaultTTL      = 15 * time.Minute
)

type Claims struct {
	DeploymentID         string `json:"deployment_id"`
	ServiceID            string `json:"service_id"`
	NodeID               string `json:"node_id"`
	CredentialGeneration uint64 `json:"credential_generation"`
	jwt.RegisteredClaims
}

type IssueRequest struct {
	DeploymentID         string
	ServiceID            string
	NodeID               string
	CredentialGeneration uint64
}

type Issuer struct {
	privateKey ed25519.PrivateKey
	publicKey  ed25519.PublicKey
	kid        string
	issuer     string
	audience   string
	ttl        time.Duration
}

type Verifier struct {
	publicKey ed25519.PublicKey
	kid       string
	issuer    string
	audience  string
}

func NewIssuer(privateKey ed25519.PrivateKey, kid, issuer, audience string, ttl time.Duration) (*Issuer, error) {
	if len(privateKey) != ed25519.PrivateKeySize {
		return nil, errors.New("workload Ed25519 private key has invalid length")
	}
	if strings.TrimSpace(kid) == "" {
		return nil, errors.New("workload key id is required")
	}
	if ttl <= 0 || ttl > time.Hour {
		return nil, errors.New("workload token TTL must be between one second and one hour")
	}
	publicKey, ok := privateKey.Public().(ed25519.PublicKey)
	if !ok {
		return nil, errors.New("derive workload public key failed")
	}
	return &Issuer{
		privateKey: privateKey,
		publicKey:  publicKey,
		kid:        strings.TrimSpace(kid),
		issuer:     firstNonEmpty(issuer, DefaultIssuer),
		audience:   firstNonEmpty(audience, DefaultAudience),
		ttl:        ttl,
	}, nil
}

func NewIssuerFromPEMFile(path, kid, issuer, audience string, ttl time.Duration) (*Issuer, error) {
	privateKey, err := ReadPrivateKeyPEM(path)
	if err != nil {
		return nil, err
	}
	return NewIssuer(privateKey, kid, issuer, audience, ttl)
}

func NewVerifier(publicKey ed25519.PublicKey, kid, issuer, audience string) (*Verifier, error) {
	if len(publicKey) != ed25519.PublicKeySize {
		return nil, errors.New("workload Ed25519 public key has invalid length")
	}
	if strings.TrimSpace(kid) == "" {
		return nil, errors.New("workload key id is required")
	}
	return &Verifier{
		publicKey: publicKey,
		kid:       strings.TrimSpace(kid),
		issuer:    firstNonEmpty(issuer, DefaultIssuer),
		audience:  firstNonEmpty(audience, DefaultAudience),
	}, nil
}

func NewVerifierFromPEMFile(path, kid, issuer, audience string) (*Verifier, error) {
	publicKey, err := ReadPublicKeyPEM(path)
	if err != nil {
		return nil, err
	}
	return NewVerifier(publicKey, kid, issuer, audience)
}

func (i *Issuer) Issue(request IssueRequest, now time.Time) (string, time.Time, error) {
	if i == nil {
		return "", time.Time{}, errors.New("workload token issuer is not configured")
	}
	request.DeploymentID = strings.TrimSpace(request.DeploymentID)
	request.ServiceID = strings.TrimSpace(request.ServiceID)
	request.NodeID = strings.TrimSpace(request.NodeID)
	if request.DeploymentID == "" || request.ServiceID == "" || request.NodeID == "" {
		return "", time.Time{}, errors.New("deployment_id, service_id and node_id are required")
	}
	if request.CredentialGeneration == 0 {
		return "", time.Time{}, errors.New("credential_generation must be positive")
	}
	now = now.UTC()
	expiresAt := now.Add(i.ttl)
	jtiBytes := make([]byte, 18)
	if _, err := rand.Read(jtiBytes); err != nil {
		return "", time.Time{}, fmt.Errorf("generate workload token id: %w", err)
	}
	claims := Claims{
		DeploymentID:         request.DeploymentID,
		ServiceID:            request.ServiceID,
		NodeID:               request.NodeID,
		CredentialGeneration: request.CredentialGeneration,
		RegisteredClaims: jwt.RegisteredClaims{
			Issuer:    i.issuer,
			Subject:   request.DeploymentID,
			Audience:  jwt.ClaimStrings{i.audience},
			ExpiresAt: jwt.NewNumericDate(expiresAt),
			NotBefore: jwt.NewNumericDate(now.Add(-5 * time.Second)),
			IssuedAt:  jwt.NewNumericDate(now),
			ID:        base64.RawURLEncoding.EncodeToString(jtiBytes),
		},
	}
	token := jwt.NewWithClaims(jwt.SigningMethodEdDSA, claims)
	token.Header["kid"] = i.kid
	signed, err := token.SignedString(i.privateKey)
	if err != nil {
		return "", time.Time{}, fmt.Errorf("sign workload token: %w", err)
	}
	return signed, expiresAt, nil
}

func (v *Verifier) Verify(tokenString string, now time.Time) (*Claims, error) {
	if v == nil {
		return nil, errors.New("workload token verifier is not configured")
	}
	claims := &Claims{}
	token, err := jwt.ParseWithClaims(
		strings.TrimSpace(tokenString),
		claims,
		func(token *jwt.Token) (any, error) {
			if token.Method != jwt.SigningMethodEdDSA {
				return nil, fmt.Errorf("unexpected workload signing method %s", token.Method.Alg())
			}
			if kid, _ := token.Header["kid"].(string); kid != v.kid {
				return nil, errors.New("unknown workload key id")
			}
			return v.publicKey, nil
		},
		jwt.WithIssuer(v.issuer),
		jwt.WithAudience(v.audience),
		jwt.WithTimeFunc(func() time.Time { return now.UTC() }),
		jwt.WithValidMethods([]string{jwt.SigningMethodEdDSA.Alg()}),
	)
	if err != nil {
		return nil, err
	}
	if !token.Valid || claims.Subject != claims.DeploymentID || claims.CredentialGeneration == 0 ||
		strings.TrimSpace(claims.ServiceID) == "" || strings.TrimSpace(claims.NodeID) == "" {
		return nil, errors.New("invalid workload token claims")
	}
	return claims, nil
}

func (i *Issuer) JWKS() map[string]any {
	return jwks(i.kid, i.publicKey)
}

// Verifier returns an in-process verifier with exactly the issuer's key and
// claim policy. Auth uses this for provider-side validation of the same
// short-lived workload JWTs it issues for Gateway-bound deployments.
func (i *Issuer) Verifier() *Verifier {
	if i == nil {
		return nil
	}
	return &Verifier{
		publicKey: append(ed25519.PublicKey(nil), i.publicKey...),
		kid:       i.kid,
		issuer:    i.issuer,
		audience:  i.audience,
	}
}

func (v *Verifier) JWKS() map[string]any {
	return jwks(v.kid, v.publicKey)
}

func jwks(kid string, publicKey ed25519.PublicKey) map[string]any {
	return map[string]any{
		"keys": []map[string]any{{
			"kty": "OKP",
			"crv": "Ed25519",
			"use": "sig",
			"alg": "EdDSA",
			"kid": kid,
			"x":   base64.RawURLEncoding.EncodeToString(publicKey),
		}},
	}
}

func ReadPrivateKeyPEM(path string) (ed25519.PrivateKey, error) {
	data, err := os.ReadFile(strings.TrimSpace(path))
	if err != nil {
		return nil, fmt.Errorf("read workload private key: %w", err)
	}
	block, _ := pem.Decode(data)
	if block == nil {
		return nil, errors.New("workload private key is not PEM")
	}
	parsed, err := x509.ParsePKCS8PrivateKey(block.Bytes)
	if err != nil {
		return nil, fmt.Errorf("parse workload private key: %w", err)
	}
	key, ok := parsed.(ed25519.PrivateKey)
	if !ok {
		return nil, errors.New("workload private key is not Ed25519")
	}
	return key, nil
}

func ReadPublicKeyPEM(path string) (ed25519.PublicKey, error) {
	data, err := os.ReadFile(strings.TrimSpace(path))
	if err != nil {
		return nil, fmt.Errorf("read workload public key: %w", err)
	}
	block, _ := pem.Decode(data)
	if block == nil {
		return nil, errors.New("workload public key is not PEM")
	}
	parsed, err := x509.ParsePKIXPublicKey(block.Bytes)
	if err != nil {
		return nil, fmt.Errorf("parse workload public key: %w", err)
	}
	key, ok := parsed.(ed25519.PublicKey)
	if !ok {
		return nil, errors.New("workload public key is not Ed25519")
	}
	return key, nil
}

func MarshalJWKS(value map[string]any) ([]byte, error) {
	return json.Marshal(value)
}

func firstNonEmpty(value, fallback string) string {
	if value = strings.TrimSpace(value); value != "" {
		return value
	}
	return fallback
}
