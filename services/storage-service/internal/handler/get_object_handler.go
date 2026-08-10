// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package handler

import (
	"context"
	"net/http"

	"github.com/zeromicro/go-zero/core/logx"
	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-storage-service/internal/logic"
	"ojos-storage-service/internal/svc"
	"ojos-storage-service/internal/types"
)

func getObjectHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ObjectReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}

		stream := &objectResponseWriter{ResponseWriter: w}
		l := logic.NewGetObjectLogic(r.Context(), svcCtx)
		err := l.GetObject(stream, r, &req)
		if err != nil {
			handleObjectStreamError(r.Context(), w, stream, err)
		}
	}
}

func handleObjectStreamError(ctx context.Context, w http.ResponseWriter, stream *objectResponseWriter, err error) {
	if stream.committed {
		// Once object bytes are visible, appending a JSON error would corrupt
		// the artifact. The declared Content-Length makes the short response
		// observable to the caller, and net/http will not reuse the connection.
		logx.WithContext(ctx).Errorf("object stream failed after response commit: %v", err)
		return
	}
	clearObjectHeaders(w.Header())
	httpx.ErrorCtx(ctx, w, err)
}

type objectResponseWriter struct {
	http.ResponseWriter
	committed bool
}

func (w *objectResponseWriter) WriteHeader(status int) {
	w.committed = true
	w.ResponseWriter.WriteHeader(status)
}

func (w *objectResponseWriter) Write(body []byte) (int, error) {
	w.committed = true
	return w.ResponseWriter.Write(body)
}

// Unwrap preserves optional interfaces for http.ResponseController.
func (w *objectResponseWriter) Unwrap() http.ResponseWriter {
	return w.ResponseWriter
}

func clearObjectHeaders(header http.Header) {
	for _, name := range []string{
		"Content-Length",
		"Content-Type",
		"X-OJOS-Object-Sha256",
	} {
		header.Del(name)
	}
}
