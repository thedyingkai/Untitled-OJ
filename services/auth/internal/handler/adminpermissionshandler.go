package handler

import (
	"net/http"

	"github.com/zeromicro/go-zero/rest/httpx"
	"ojos-auth/internal/logic"
	"ojos-auth/internal/svc"
	"ojos-auth/internal/types"
)

func listUsersHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminPermissionsLogic(r.Context(), svcCtx)
		resp, err := l.ListUsers()
		writeResp(r, w, resp, err)
	}
}

func listRolesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminPermissionsLogic(r.Context(), svcCtx)
		resp, err := l.ListRoles()
		writeResp(r, w, resp, err)
	}
}

func listPermissionsHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminPermissionsLogic(r.Context(), svcCtx)
		resp, err := l.ListPermissions()
		writeResp(r, w, resp, err)
	}
}

func addUserRoleHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.UserRoleReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		l := logic.NewAdminPermissionsLogic(r.Context(), svcCtx)
		resp, err := l.AddUserRole(&req)
		writeResp(r, w, resp, err)
	}
}

func removeUserRoleHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.UserRoleReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		l := logic.NewAdminPermissionsLogic(r.Context(), svcCtx)
		resp, err := l.RemoveUserRole(&req)
		writeResp(r, w, resp, err)
	}
}

func addProblemRoleHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ProblemRoleReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		l := logic.NewAdminPermissionsLogic(r.Context(), svcCtx)
		resp, err := l.AddProblemRole(&req)
		writeResp(r, w, resp, err)
	}
}

func removeProblemRoleHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ProblemRoleReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		l := logic.NewAdminPermissionsLogic(r.Context(), svcCtx)
		resp, err := l.RemoveProblemRole(&req)
		writeResp(r, w, resp, err)
	}
}

func permissionCheckHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.PermissionCheckReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		l := logic.NewAdminPermissionsLogic(r.Context(), svcCtx)
		resp, err := l.CheckPermission(&req)
		writeResp(r, w, resp, err)
	}
}

func listAuditLogsHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		l := logic.NewAdminPermissionsLogic(r.Context(), svcCtx)
		resp, err := l.ListAuditLogs()
		writeResp(r, w, resp, err)
	}
}

func writeResp(r *http.Request, w http.ResponseWriter, resp any, err error) {
	if err != nil {
		httpx.ErrorCtx(r.Context(), w, err)
		return
	}
	httpx.OkJsonCtx(r.Context(), w, resp)
}
