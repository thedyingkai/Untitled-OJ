package handler

import (
	"encoding/json"
	"net/http"
	"strings"

	orchestratorsnapshot "ojos-gateway/internal/orchestrator/snapshot"
	"ojos-gateway/internal/svc"
)

// contributionSnapshotHandler exposes the exact active snapshot through the
// user Shell origin. The Gateway fetches it with its control-plane credential;
// browser credentials and Orchestrator tokens are never forwarded upstream.
func contributionSnapshotHandler(serviceContext *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if serviceContext == nil || serviceContext.Orchestrator == nil || !serviceContext.Orchestrator.Configured() {
			writeContributionProblem(w, http.StatusServiceUnavailable, "CONTRIBUTION_SNAPSHOT_UNAVAILABLE", "Contribution snapshot consumer is not configured")
			return
		}
		snapshot, err := serviceContext.Orchestrator.ContributionSnapshot(r.Context())
		if err != nil {
			writeContributionProblem(w, http.StatusServiceUnavailable, "CONTRIBUTION_SNAPSHOT_UNAVAILABLE", "active Contribution snapshot could not be loaded")
			return
		}
		snapshot = userContributionSnapshot(snapshot)
		w.Header().Set("Content-Type", "application/json; charset=utf-8")
		w.Header().Set("Cache-Control", "private, no-cache")
		w.Header().Set("ETag", snapshot.Digest)
		_ = json.NewEncoder(w).Encode(map[string]any{
			"data": snapshot,
			"meta": map[string]any{"api_version": "v1"},
		})
	}
}

// userContributionSnapshot deliberately removes control-plane topology,
// permission-definition and admin-shell data before the document crosses the
// browser trust boundary. The user Shell only needs its own signed modules and
// the public/user operation registry; upstream addresses remain server-side.
func userContributionSnapshot(snapshot orchestratorsnapshot.ContributionSnapshot) orchestratorsnapshot.ContributionSnapshot {
	result := orchestratorsnapshot.ContributionSnapshot{
		SchemaVersion:         snapshot.SchemaVersion,
		Digest:                snapshot.Digest,
		ScopeID:               snapshot.ScopeID,
		Revisions:             []orchestratorsnapshot.ContributionRevision{},
		GatewayRoutes:         []orchestratorsnapshot.ContributionGatewayRoute{},
		PermissionDefinitions: []orchestratorsnapshot.ContributionPermissionDefinition{},
		UserFrontendModules:   append([]orchestratorsnapshot.ContributionFrontendModule(nil), snapshot.UserFrontendModules...),
		AdminFrontendModules:  []orchestratorsnapshot.ContributionFrontendModule{},
	}
	for _, route := range snapshot.GatewayRoutes {
		audience := strings.ToLower(strings.TrimSpace(route.Audience))
		if audience != "user" && audience != "public" {
			continue
		}
		route.UpstreamBase = ""
		result.GatewayRoutes = append(result.GatewayRoutes, route)
	}
	return result
}

func writeContributionProblem(w http.ResponseWriter, status int, code, detail string) {
	w.Header().Set("Content-Type", "application/problem+json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(map[string]any{
		"type":   "urn:ojos:problem:contribution-snapshot-unavailable",
		"title":  code,
		"status": status,
		"detail": detail,
	})
}
