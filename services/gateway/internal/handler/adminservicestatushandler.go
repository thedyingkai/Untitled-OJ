package handler

import (
	"net/http"

	"ojos-gateway/internal/logic"
	"ojos-gateway/internal/svc"

	"github.com/zeromicro/go-zero/rest/httpx"
	"github.com/zeromicro/go-zero/rest/pathvar"
)

func adminServiceStatusServicesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminServiceStatusLogic(r.Context(), svcCtx)
		resp, err := l.ListServices(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminServiceStatusOperationsHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminServiceStatusLogic(r.Context(), svcCtx)
		resp, err := l.Operations(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminServiceStatusOperationDetailHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminServiceStatusLogic(r.Context(), svcCtx)
		resp, err := l.OperationDetail(r.Header.Get("Authorization"), pathvar.Vars(r)["id"])
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminServiceStatusServiceDetailHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminServiceStatusLogic(r.Context(), svcCtx)
		resp, err := l.ServiceDetail(r.Header.Get("Authorization"), serviceStatusServiceID(r))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func serviceStatusServiceID(r *http.Request) string {
	return pathvar.Vars(r)["id"]
}
