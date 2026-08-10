// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package handler

import (
	"errors"
	"net/http"
	"strings"

	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-storage-service/internal/logic"
	"ojos-storage-service/internal/store"
	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"
)

func putObjectHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ObjectReq
		if err := httpx.ParsePath(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}

		l := logic.NewPutObjectLogic(r.Context(), svcCtx)
		if precondition := strings.TrimSpace(r.Header.Get("If-None-Match")); precondition != "" && precondition != "*" {
			http.Error(w, "only If-None-Match: * is supported", http.StatusBadRequest)
			return
		}
		resp, err := l.PutObject(&req, store.PutOptions{
			ContentType:    r.Header.Get("Content-Type"),
			SizeBytes:      r.ContentLength,
			SizeKnown:      r.ContentLength >= 0,
			ExpectedSHA256: r.Header.Get("X-OJOS-Content-Sha256"),
			IfAbsent:       strings.TrimSpace(r.Header.Get("If-None-Match")) == "*",
		}, r.Body)
		if err != nil {
			if errors.Is(err, store.ErrPreconditionFailed) {
				http.Error(w, err.Error(), http.StatusPreconditionFailed)
				return
			}
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}
