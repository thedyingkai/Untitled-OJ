// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package handler

import (
	"net/http"

	"ojos-problem-api/internal/logic"
	"ojos-problem-api/internal/svc"
	"ojos-problem-api/internal/types"

	"github.com/zeromicro/go-zero/rest/httpx"
)

func listPackageCasesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ListPackageCasesReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}

		l := logic.NewListPackageCasesLogic(r.Context(), svcCtx)
		resp, err := l.ListPackageCases(&req)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}
