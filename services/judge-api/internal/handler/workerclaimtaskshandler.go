package handler

import (
	"errors"
	"net/http"
	"strings"
	"time"

	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-judge-api/internal/logic"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
)

func workerClaimTasksHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.WorkerClaimTasksReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}

		l := logic.NewWorkerClaimTasksLogic(r.Context(), svcCtx)
		wait := time.Duration(0)
		if preference := strings.TrimSpace(r.Header.Get("Prefer")); preference != "" {
			if preference != "wait=25" {
				httpx.ErrorCtx(r.Context(), w, errors.New("Prefer must be exactly wait=25"))
				return
			}
			wait = 25 * time.Second
			w.Header().Set("Preference-Applied", "wait=25")
		}
		resp, err := l.WorkerClaimTasksWithWait(&req, wait)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}
