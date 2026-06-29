package handler

import (
	"net/http"

	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-gateway/internal/logic"
	"ojos-gateway/internal/svc"
)

func adminServicesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminServicesLogic(r.Context(), svcCtx)
		resp, err := l.ListServices(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminServiceSetsHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminServicesLogic(r.Context(), svcCtx)
		resp, err := l.ListSets(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminServiceTopologyHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminServicesLogic(r.Context(), svcCtx)
		resp, err := l.Topology(r.Header.Get("Authorization"))
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminOrchestratorSnapshotHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminServicesLogic(r.Context(), svcCtx)
		includeDisabled := r.URL.Query().Get("include_disabled") == "true"
		resp, err := l.OrchestratorSnapshot(r.Header.Get("Authorization"), includeDisabled)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminOrchestratorRoutesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminServicesLogic(r.Context(), svcCtx)
		includeDisabled := r.URL.Query().Get("include_disabled") == "true"
		includeUpstream := r.URL.Query().Get("debug_upstream") == "true"
		resp, err := l.OrchestratorRoutes(r.Header.Get("Authorization"), includeDisabled, includeUpstream)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}

func adminServiceDetailHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req struct {
			Id string `path:"id"`
		}
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}

		l := logic.NewAdminServicesLogic(r.Context(), svcCtx)
		resp, err := l.Detail(r.Header.Get("Authorization"), req.Id)
		if err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
		} else {
			httpx.OkJsonCtx(r.Context(), w, resp)
		}
	}
}
