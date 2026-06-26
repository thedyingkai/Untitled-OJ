package handler

import (
	"net/http"

	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-judge-api/internal/logic"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
)

func workerArtifactSubmissionSourceHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.WorkerArtifactSourceReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}

		if err := logic.ServeWorkerSubmissionSource(r.Context(), svcCtx, w, r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		}
	}
}

func workerArtifactProblemPackageHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.WorkerArtifactProblemPackageReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}

		if err := logic.ServeWorkerProblemPackage(r.Context(), svcCtx, w, r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		}
	}
}
