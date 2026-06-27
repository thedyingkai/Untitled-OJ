package handler

import (
	"encoding/json"
	"errors"
	"io"
	"net/http"

	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-gateway/internal/logic"
	"ojos-gateway/internal/svc"
	"ojos-gateway/internal/types"
)

func adminModuleInstallerDiscoverHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminModuleInstallerLogic(r.Context(), svcCtx)
		resp, err := l.Discover(r.Header.Get("Authorization"))
		writeModuleInstallerResp(r, w, resp, err)
	}
}

func adminModuleInstallerValidateHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return installerReqHandler(svcCtx, func(l *logic.AdminModuleInstallerLogic, auth string, req *types.ModuleInstallerReq) (*types.ModuleInstallerResp, error) {
		return l.Validate(auth, req)
	})
}

func adminModuleInstallerPlanHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return installerReqHandler(svcCtx, func(l *logic.AdminModuleInstallerLogic, auth string, req *types.ModuleInstallerReq) (*types.ModuleInstallerResp, error) {
		return l.Plan(auth, req)
	})
}

func adminModuleInstallerInstallHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return installerReqHandler(svcCtx, func(l *logic.AdminModuleInstallerLogic, auth string, req *types.ModuleInstallerReq) (*types.ModuleInstallerResp, error) {
		return l.Install(auth, req)
	})
}

func adminModuleInstallerEnableHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return installerActionHandler(svcCtx, "/enable", http.MethodPost, nil)
}

func adminModuleInstallerDisableHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return installerActionHandler(svcCtx, "/disable", http.MethodPost, nil)
}

func adminModuleInstallerUpgradePlanHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var pathReq struct {
			Id string `path:"id"`
		}
		var req types.ModuleInstallerReq
		if err := httpx.Parse(r, &pathReq); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		if err := decodeJSONBody(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		l := logic.NewAdminModuleInstallerLogic(r.Context(), svcCtx)
		resp, err := l.ModuleAction(r.Header.Get("Authorization"), pathReq.Id, "/upgrade-plan", http.MethodPost, &req)
		writeModuleInstallerResp(r, w, resp, err)
	}
}

func adminModuleInstallerRollbackPlanHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return installerActionHandler(svcCtx, "/rollback-plan", http.MethodPost, map[string]any{})
}

func adminModuleInstallerUninstallDryRunHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return installerActionHandler(svcCtx, "/uninstall-dry-run", http.MethodPost, map[string]any{})
}

func adminModuleInstallerHealthHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return installerActionHandler(svcCtx, "/health", http.MethodGet, nil)
}

func adminModuleInstallerOperationsHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return installerActionHandler(svcCtx, "/operations", http.MethodGet, nil)
}

func installerReqHandler(
	svcCtx *svc.ServiceContext,
	call func(*logic.AdminModuleInstallerLogic, string, *types.ModuleInstallerReq) (*types.ModuleInstallerResp, error),
) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ModuleInstallerReq
		if err := decodeJSONBody(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		l := logic.NewAdminModuleInstallerLogic(r.Context(), svcCtx)
		resp, err := call(l, r.Header.Get("Authorization"), &req)
		writeModuleInstallerResp(r, w, resp, err)
	}
}

func decodeJSONBody(r *http.Request, dest any) error {
	if r.Body == nil {
		return nil
	}
	defer r.Body.Close()
	err := json.NewDecoder(r.Body).Decode(dest)
	if errors.Is(err, io.EOF) {
		return nil
	}
	return err
}

func installerActionHandler(svcCtx *svc.ServiceContext, suffix string, method string, body any) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req struct {
			Id string `path:"id"`
		}
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		l := logic.NewAdminModuleInstallerLogic(r.Context(), svcCtx)
		resp, err := l.ModuleAction(r.Header.Get("Authorization"), req.Id, suffix, method, body)
		writeModuleInstallerResp(r, w, resp, err)
	}
}

func writeModuleInstallerResp(r *http.Request, w http.ResponseWriter, resp *types.ModuleInstallerResp, err error) {
	if err != nil {
		httpx.ErrorCtx(r.Context(), w, err)
		return
	}
	httpx.OkJsonCtx(r.Context(), w, resp)
}
