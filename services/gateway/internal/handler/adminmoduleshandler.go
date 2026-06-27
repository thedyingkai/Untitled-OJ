package handler

import (
	"net/http"

	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-gateway/internal/logic"
	"ojos-gateway/internal/svc"
)

func adminModulesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminModulesLogic(r.Context(), svcCtx)
		resp, err := l.ListModules(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminModuleSetsHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminModulesLogic(r.Context(), svcCtx)
		resp, err := l.ListSets(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminModuleTopologyHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminModulesLogic(r.Context(), svcCtx)
		resp, err := l.Topology(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminModuleRuntimeSnapshotHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminModulesLogic(r.Context(), svcCtx)
		includeDisabled := r.URL.Query().Get("include_disabled") == "true"
		resp, err := l.RuntimeSnapshot(r.Header.Get("Authorization"), includeDisabled)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminModuleRuntimeRoutesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminModulesLogic(r.Context(), svcCtx)
		includeDisabled := r.URL.Query().Get("include_disabled") == "true"
		includeUpstream := r.URL.Query().Get("debug_upstream") == "true"
		resp, err := l.RuntimeRoutes(r.Header.Get("Authorization"), includeDisabled, false, includeUpstream)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminModuleRuntimeReloadHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminModulesLogic(r.Context(), svcCtx)
		includeDisabled := r.URL.Query().Get("include_disabled") == "true"
		includeUpstream := r.URL.Query().Get("debug_upstream") == "true"
		resp, err := l.RuntimeRoutes(r.Header.Get("Authorization"), includeDisabled, true, includeUpstream)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminModuleDetailHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req struct {
			Id string `path:"id"`
		}
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}

		l := logic.NewAdminModulesLogic(r.Context(), svcCtx)
		resp, err := l.Detail(r.Header.Get("Authorization"), req.Id)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}
