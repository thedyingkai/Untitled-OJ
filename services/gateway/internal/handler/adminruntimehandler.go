package handler

import (
	"encoding/json"
	"net/http"

	"ojos-gateway/internal/logic"
	"ojos-gateway/internal/svc"

	"github.com/zeromicro/go-zero/rest/httpx"
	"github.com/zeromicro/go-zero/rest/pathvar"
)

func adminRuntimeServicesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminRuntimeLogic(r.Context(), svcCtx)
		resp, err := l.ListServices(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminRuntimeOperationsHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminRuntimeLogic(r.Context(), svcCtx)
		resp, err := l.Operations(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminRuntimeOperationDetailHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminRuntimeLogic(r.Context(), svcCtx)
		resp, err := l.OperationDetail(r.Header.Get("Authorization"), pathvar.Vars(r)["id"])
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminRuntimeServiceDetailHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminRuntimeLogic(r.Context(), svcCtx)
		resp, err := l.ServiceDetail(r.Header.Get("Authorization"), runtimeServiceID(r))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminRuntimeServicePlanHandler(svcCtx *svc.ServiceContext, action string) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminRuntimeLogic(r.Context(), svcCtx)
		var (
			resp any
			err  error
		)
		switch action {
		case "start":
			resp, err = l.PlanStart(r.Header.Get("Authorization"), runtimeServiceID(r))
		case "stop":
			resp, err = l.PlanStop(r.Header.Get("Authorization"), runtimeServiceID(r))
		case "restart":
			resp, err = l.PlanRestart(r.Header.Get("Authorization"), runtimeServiceID(r))
		default:
			resp, err = l.PlanReload(r.Header.Get("Authorization"), runtimeServiceID(r))
		}
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func runtimeServiceID(r *http.Request) string {
	return pathvar.Vars(r)["id"]
}

func adminRuntimeReloadHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminRuntimeLogic(r.Context(), svcCtx)
		resp, err := l.Reload(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminRuntimeApplyPlanDisabledHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminRuntimeLogic(r.Context(), svcCtx)
		resp, err := l.ApplyDisabled(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusNotImplemented)
		_ = json.NewEncoder(w).Encode(resp)
	}
}
