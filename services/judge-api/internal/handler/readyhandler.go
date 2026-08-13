package handler

import (
	"encoding/json"
	"net/http"

	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/zeromicro/go-zero/rest/httpx"
)

func readyHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if err := svcCtx.Ready(r.Context()); err != nil {
			w.Header().Set("Content-Type", "application/json; charset=utf-8")
			w.WriteHeader(http.StatusServiceUnavailable)
			_ = json.NewEncoder(w).Encode(map[string]string{
				"code": "NOT_READY", "message": err.Error(),
			})
			return
		}
		httpx.OkJsonCtx(r.Context(), w, &types.HealthResp{Status: "ok"})
	}
}
