// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package handler

import (
	"net/http"

	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-problem-service/internal/logic"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"
)

func reconcileArtifactGCIntentHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ReconcileArtifactGCIntentReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		req.IdempotencyKey = r.Header.Get("Idempotency-Key")

		l := logic.NewReconcileArtifactGCIntentLogic(r.Context(), svcCtx)
		resp, err := l.ReconcileArtifactGCIntent(&req)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.WriteJson(w, http.StatusAccepted, resp)
		}
	}
}
