package handler

import (
	"encoding/json"
	"errors"
	"net/http"

	"ojos-problem-service/internal/logic"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/rest/httpx"
)

func readyHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if err := svcCtx.Ready(r.Context()); err != nil {
			w.Header().Set("Content-Type", "application/json; charset=utf-8")
			w.WriteHeader(http.StatusServiceUnavailable)
			_ = json.NewEncoder(w).Encode(map[string]string{"code": "NOT_READY", "message": err.Error()})
			return
		}
		httpx.OkJsonCtx(r.Context(), w, &types.HealthResp{Status: "ok"})
	}
}

func adminListProblemsHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		user, ok := authctx.FromContext(r.Context())
		if !ok || user == nil || user.UserID <= 0 {
			writeContractError(w, http.StatusUnauthorized, "UNAUTHORIZED", "unauthorized")
			return
		}
		checker := svcCtx.ActivePermissionChecker()
		if checker == nil {
			writeContractError(w, http.StatusServiceUnavailable, "PERMISSION_UNAVAILABLE", "permission checker is unavailable")
			return
		}
		if err := checker.RequireUserPermission(r.Context(), user.UserID, "problem.edit", sharedperm.SystemScope()); err != nil {
			if errors.Is(err, sharedperm.ErrForbidden) {
				writeContractError(w, http.StatusForbidden, "FORBIDDEN", "permission denied")
			} else {
				writeContractError(w, http.StatusServiceUnavailable, "PERMISSION_UNAVAILABLE", "permission checker is unavailable")
			}
			return
		}
		var req types.ListProblemsReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		response, err := logic.NewListProblemsLogic(r.Context(), svcCtx).ListProblemsAuthorized(&req)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		httpx.OkJsonCtx(r.Context(), w, response)
	}
}

func writeContractError(w http.ResponseWriter, status int, code, message string) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(map[string]string{"code": code, "message": message})
}
