package handler

import (
	"crypto/subtle"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"

	"ojos-gateway/internal/svc"
	shared "ojos-shared/topologyprojection"

	"github.com/zeromicro/go-zero/rest/httpx"
)

const topologyRequestLimit = 8 * 1024 * 1024

type topologyPath struct {
	TopologyID string `path:"id"`
}

func topologyProjectionHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !validTopologyManagementToken(r, svcCtx.Config.Orchestrator.InternalToken) {
			writeTopologyProblem(w, http.StatusUnauthorized, "TOPOLOGY_PROVIDER_UNAUTHORIZED", "valid Orchestrator management bearer is required")
			return
		}
		var path topologyPath
		if err := httpx.ParsePath(r, &path); err != nil || strings.TrimSpace(path.TopologyID) == "" {
			writeTopologyProblem(w, http.StatusBadRequest, "TOPOLOGY_ID_INVALID", "topology id is required")
			return
		}
		switch r.Method {
		case http.MethodGet:
			document, err := svcCtx.TopologyProjection.Get(r.Context(), path.TopologyID)
			if err != nil {
				writeTopologyProblem(w, http.StatusServiceUnavailable, "TOPOLOGY_PROVIDER_STORAGE", err.Error())
				return
			}
			if document == nil {
				writeTopologyJSON(w, http.StatusOK, shared.AbsentStatus("gateway", path.TopologyID))
				return
			}
			status, err := document.Status()
			if err != nil {
				writeTopologyProblem(w, http.StatusInternalServerError, "TOPOLOGY_PROVIDER_CORRUPT", err.Error())
				return
			}
			writeTopologyJSON(w, http.StatusOK, status)
		case http.MethodPut, http.MethodDelete:
			request, err := readTopologyRequest(r)
			if err != nil {
				writeTopologyProblem(w, http.StatusBadRequest, "TOPOLOGY_REQUEST_INVALID", err.Error())
				return
			}
			if err := request.Validate("gateway", path.TopologyID); err != nil {
				writeTopologyProblem(w, http.StatusUnprocessableEntity, "TOPOLOGY_REQUEST_INVALID", err.Error())
				return
			}
			if r.Method == http.MethodDelete && request.Action != "delete" {
				writeTopologyProblem(w, http.StatusUnprocessableEntity, "TOPOLOGY_ACTION_INVALID", "DELETE requires action=delete")
				return
			}
			if r.Method == http.MethodPut && request.Action == "delete" {
				writeTopologyProblem(w, http.StatusUnprocessableEntity, "TOPOLOGY_ACTION_INVALID", "PUT cannot use action=delete")
				return
			}
			if request.Action == "delete" {
				err = svcCtx.TopologyProjection.Delete(r.Context(), path.TopologyID)
			} else {
				err = svcCtx.TopologyProjection.Apply(r.Context(), request)
			}
			if err != nil {
				writeTopologyProblem(w, http.StatusServiceUnavailable, "TOPOLOGY_PROVIDER_STORAGE", err.Error())
				return
			}
			writeTopologyJSON(w, http.StatusOK, shared.AckFor(request, request.Action == "delete"))
		default:
			w.Header().Set("Allow", "GET, PUT, DELETE")
			writeTopologyProblem(w, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "method is not supported")
		}
	}
}

func readTopologyRequest(r *http.Request) (shared.Request, error) {
	if !strings.HasPrefix(strings.ToLower(strings.TrimSpace(r.Header.Get("Content-Type"))), "application/json") {
		return shared.Request{}, errors.New("Content-Type must be application/json")
	}
	data, err := io.ReadAll(io.LimitReader(r.Body, topologyRequestLimit+1))
	if err != nil {
		return shared.Request{}, errors.New("request body could not be read")
	}
	if len(data) > topologyRequestLimit {
		return shared.Request{}, errors.New("request body exceeds configured limit")
	}
	return shared.DecodeRequest(data)
}

func validTopologyManagementToken(r *http.Request, expected string) bool {
	expected = strings.TrimSpace(expected)
	header := strings.TrimSpace(r.Header.Get("Authorization"))
	parts := strings.Fields(header)
	if expected == "" || len(parts) != 2 || !strings.EqualFold(parts[0], "Bearer") || len(parts[1]) != len(expected) {
		return false
	}
	return subtle.ConstantTimeCompare([]byte(parts[1]), []byte(expected)) == 1
}

func writeTopologyJSON(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}

func writeTopologyProblem(w http.ResponseWriter, status int, code, detail string) {
	w.Header().Set("Content-Type", "application/problem+json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(map[string]any{
		"type": "urn:ojos:problem:" + strings.ToLower(code), "title": code,
		"status": status, "detail": detail,
	})
}
