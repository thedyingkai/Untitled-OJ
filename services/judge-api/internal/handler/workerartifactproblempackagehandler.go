// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package handler

import (
	"net/http"

	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-judge-api/internal/logic"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
)

func workerArtifactProblemPackageHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.WorkerArtifactProblemPackageReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}

		l := logic.NewWorkerArtifactProblemPackageLogic(r.Context(), svcCtx)
		if err := l.Serve(w, r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		}
	}
}
