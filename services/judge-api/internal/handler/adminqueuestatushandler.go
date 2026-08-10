package handler

import (
	"net/http"

	"ojos-judge-api/internal/svc"
)

// adminQueueStatusHandler keeps the two public route identities distinct for
// goctl while intentionally sharing the same queue projection implementation.
func adminQueueStatusHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return adminQueueHandler(svcCtx)
}
