package handler

import (
	"encoding/json"
	"errors"
	"net/http"

	"github.com/zeromicro/go-zero/rest/httpx"

	"ojos-auth-service/internal/service"
	"ojos-auth-service/internal/svc"
	"ojos-auth-service/internal/types"
)

func adminBootstrapHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.AdminBootstrapReq
		if err := httpx.Parse(r, &req); err != nil {
			writeAdminBootstrapJSON(w, http.StatusBadRequest, types.AdminBootstrapResp{
				Code: 40031,
				Msg:  "invalid bootstrap request",
			})
			return
		}

		result, err := svcCtx.AdminBootstrap.Bootstrap(r.Context(), service.AdminBootstrapRequest{
			Secret:   req.BootstrapSecret,
			Username: req.Username,
			Email:    req.Email,
			Password: req.Password,
		})
		if err != nil {
			switch {
			case errors.Is(err, service.ErrInvalidAdminBootstrapSecret):
				writeAdminBootstrapJSON(w, http.StatusForbidden, types.AdminBootstrapResp{
					Code: 40331,
					Msg:  "invalid bootstrap credential",
				})
			case service.IsAdminBootstrapConsumed(err):
				writeAdminBootstrapJSON(w, http.StatusConflict, types.AdminBootstrapResp{
					Code: 40931,
					Msg:  "initial administrator bootstrap is unavailable",
				})
			case service.IsAdminBootstrapUserExists(err):
				writeAdminBootstrapJSON(w, http.StatusConflict, types.AdminBootstrapResp{
					Code: 40932,
					Msg:  "bootstrap user already exists",
				})
			case errors.Is(err, service.ErrInvalidInput):
				writeAdminBootstrapJSON(w, http.StatusBadRequest, types.AdminBootstrapResp{
					Code: 40031,
					Msg:  err.Error(),
				})
			default:
				svcCtx.Logger.Error("initial administrator bootstrap failed")
				writeAdminBootstrapJSON(w, http.StatusInternalServerError, types.AdminBootstrapResp{
					Code: 50031,
					Msg:  "initial administrator bootstrap failed",
				})
			}
			return
		}

		writeAdminBootstrapJSON(w, http.StatusCreated, types.AdminBootstrapResp{
			Code: 0,
			Msg:  "success",
			Data: types.AdminBootstrapData{
				UserId:   result.UserID,
				Username: result.Username,
			},
		})
	}
}

func writeAdminBootstrapJSON(w http.ResponseWriter, status int, response types.AdminBootstrapResp) {
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(response)
}
