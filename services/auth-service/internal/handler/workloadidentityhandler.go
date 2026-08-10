package handler

import (
	"crypto/subtle"
	"encoding/json"
	"net/http"
	"strings"
	"time"

	"ojos-auth-service/internal/middleware"
	"ojos-auth-service/internal/svc"
	"ojos-shared/security/workload"
)

type workloadTokenIssueRequest struct {
	DeploymentID         string `json:"deployment_id"`
	ServiceID            string `json:"service_id"`
	NodeID               string `json:"node_id"`
	CredentialGeneration uint64 `json:"credential_generation"`
}

func workloadTokenIssueHandler(serviceContext *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		configured := strings.TrimSpace(serviceContext.Config.WorkloadIdentity.ControlPlaneToken)
		presented, _ := middleware.TokenFromContext(r.Context())
		if configured == "" || subtle.ConstantTimeCompare([]byte(configured), []byte(presented)) != 1 {
			writeWorkloadProblem(w, http.StatusForbidden, "only the control-plane internal identity may issue workload tokens")
			return
		}
		if serviceContext.WorkloadIssuer == nil {
			writeWorkloadProblem(w, http.StatusServiceUnavailable, "workload token issuer is not configured")
			return
		}
		decoder := json.NewDecoder(http.MaxBytesReader(w, r.Body, 64*1024))
		decoder.DisallowUnknownFields()
		var request workloadTokenIssueRequest
		if err := decoder.Decode(&request); err != nil {
			writeWorkloadProblem(w, http.StatusBadRequest, "invalid workload token request")
			return
		}
		token, expiresAt, err := serviceContext.WorkloadIssuer.Issue(workload.IssueRequest{
			DeploymentID:         request.DeploymentID,
			ServiceID:            request.ServiceID,
			NodeID:               request.NodeID,
			CredentialGeneration: request.CredentialGeneration,
		}, time.Now())
		if err != nil {
			writeWorkloadProblem(w, http.StatusBadRequest, err.Error())
			return
		}
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Cache-Control", "no-store")
		expiresIn := max(int64((time.Until(expiresAt)+time.Second-1)/time.Second), 0)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"access_token": token,
			"token_type":   "Bearer",
			"expires_at":   expiresAt.UTC().Format(time.RFC3339Nano),
			"expires_in":   expiresIn,
		})
	}
}

func workloadJWKSHandler(serviceContext *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		if serviceContext.WorkloadIssuer == nil {
			writeWorkloadProblem(w, http.StatusServiceUnavailable, "workload identity is not configured")
			return
		}
		w.Header().Set("Content-Type", "application/jwk-set+json")
		w.Header().Set("Cache-Control", "public, max-age=300")
		_ = json.NewEncoder(w).Encode(serviceContext.WorkloadIssuer.JWKS())
	}
}

func writeWorkloadProblem(w http.ResponseWriter, status int, detail string) {
	w.Header().Set("Content-Type", "application/problem+json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(map[string]any{
		"type":   "https://ojos.dev/problems/workload-identity",
		"title":  http.StatusText(status),
		"status": status,
		"detail": detail,
	})
}
