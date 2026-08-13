// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package handler

import (
	"errors"
	"net/http"
	"strconv"
	"strings"

	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-shared/storagecontract"
	"ojos-storage-service/internal/logic"
	"ojos-storage-service/internal/store"
	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"
)

func deleteObjectHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ObjectReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}

		l := logic.NewDeleteObjectLogic(r.Context(), svcCtx)
		expectedSHA := strings.ToLower(strings.TrimSpace(r.Header.Get("X-OJOS-Expected-Sha256")))
		expectedSizeText := strings.TrimSpace(r.Header.Get("X-OJOS-Expected-Size"))
		var resp *types.DeleteObjectResp
		var err error
		if svcCtx.WorkloadAuthEnabled && (expectedSHA == "" || expectedSizeText == "") {
			http.Error(w, "workload delete requires X-OJOS-Expected-Sha256 and X-OJOS-Expected-Size", http.StatusBadRequest)
			return
		}
		if expectedSHA == "" && expectedSizeText == "" {
			resp, err = l.DeleteObject(&req)
		} else {
			expectedSize, parseErr := strconv.ParseInt(expectedSizeText, 10, 64)
			if expectedSHA == "" || parseErr != nil || expectedSize < 0 {
				http.Error(w, "both valid X-OJOS-Expected-Sha256 and X-OJOS-Expected-Size headers are required", http.StatusBadRequest)
				return
			}
			resp, err = l.DeleteObjectIfMatches(&req, expectedSHA, expectedSize)
		}
		if err != nil {
			if errors.Is(err, store.ErrPreconditionFailed) {
				http.Error(w, "object identity precondition failed", http.StatusPreconditionFailed)
				return
			}
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			// Provider provenance prevents a successful response from another
			// Gateway route from being mistaken for this conditional deletion.
			w.Header().Set(storagecontract.ResultHeader, storagecontract.ResultDeleted)
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}
