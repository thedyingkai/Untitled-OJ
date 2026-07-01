package handler

import (
	"net/http"

	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-auth-service/internal/logic"
	"ojos-auth-service/internal/svc"
	"ojos-auth-service/internal/types"
)

func registerServicePermissionsHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.RegisterServicePermissionsReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}

		l := logic.NewServicePermissionsLogic(r.Context(), svcCtx)
		resp, err := l.Register(&req)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}
