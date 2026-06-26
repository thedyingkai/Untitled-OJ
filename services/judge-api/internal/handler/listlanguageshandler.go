// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package handler

import (
	"net/http"

	"ojos-judge-api/internal/logic"
	"ojos-judge-api/internal/svc"

	"github.com/zeromicro/go-zero/rest/httpx"
)

func listLanguagesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewListLanguagesLogic(r.Context(), svcCtx)
		resp, err := l.ListLanguages()
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}
