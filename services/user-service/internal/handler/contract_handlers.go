package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"

	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"
	"ojos-user-service/internal/store"
	"ojos-user-service/internal/svc"
	"ojos-user-service/internal/types"

	"github.com/zeromicro/go-zero/rest/httpx"
)

func readyHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if err := svcCtx.Ready(r.Context()); err != nil {
			w.Header().Set("Content-Type", "application/json; charset=utf-8")
			w.WriteHeader(http.StatusServiceUnavailable)
			_ = json.NewEncoder(w).Encode(map[string]string{
				"code": "NOT_READY", "message": err.Error(),
			})
			return
		}
		httpx.OkJsonCtx(r.Context(), w, &types.HealthResp{Status: "ok", Service: "user-service"})
	}
}

func requireOperationPermission(svcCtx *svc.ServiceContext, permission string, next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		user, ok := authctx.FromContext(r.Context())
		if !ok || user == nil || user.UserID <= 0 {
			writeContractError(w, http.StatusUnauthorized, "UNAUTHORIZED", "unauthorized")
			return
		}
		checker := svcCtx.ActivePermissionChecker()
		if checker == nil {
			writeContractError(w, http.StatusServiceUnavailable, "PERMISSION_UNAVAILABLE", "permission checker is unavailable")
			return
		}
		if err := checker.RequireUserPermission(r.Context(), user.UserID, permission, sharedperm.SystemScope()); err != nil {
			if errors.Is(err, sharedperm.ErrForbidden) {
				writeContractError(w, http.StatusForbidden, "FORBIDDEN", "permission denied")
			} else {
				writeContractError(w, http.StatusServiceUnavailable, "PERMISSION_UNAVAILABLE", "permission checker is unavailable")
			}
			return
		}
		next(w, r)
	}
}

func getMyProfileHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		user, ok := authctx.FromContext(r.Context())
		if !ok || user == nil || user.UserID <= 0 {
			writeContractError(w, http.StatusUnauthorized, "UNAUTHORIZED", "unauthorized")
			return
		}
		userID := strconv.FormatInt(user.UserID, 10)
		resp, err := svcCtx.ProfileStore.GetOrCreateCtx(r.Context(), userID, user.Username)
		writeLogicResult(r, w, &resp, err)
	}
}

func updateMeHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.MyProfilePatchReq
		if err := decodeContractJSON(w, r, &req); err != nil {
			writeContractError(w, http.StatusBadRequest, "INVALID_REQUEST", err.Error())
			return
		}
		var userID string
		if !setCurrentUserID(r, &userID) {
			writeContractError(w, http.StatusUnauthorized, "UNAUTHORIZED", "unauthorized")
			return
		}
		resp, err := svcCtx.ProfileStore.UpdateCtx(r.Context(), userID, store.ProfilePatch{
			DisplayName: req.DisplayName, Bio: req.Bio, AvatarObject: req.AvatarObject,
			Preferences: req.Preferences,
		})
		writeLogicResult(r, w, resp, err)
	}
}

func getMyPreferencesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ProfileReq
		if !setCurrentUserID(r, &req.UserId) {
			writeContractError(w, http.StatusUnauthorized, "UNAUTHORIZED", "unauthorized")
			return
		}
		profile, err := svcCtx.ProfileStore.GetOrCreateCtx(r.Context(), req.UserId, req.UserId)
		writeLogicResult(r, w, &types.PreferencesResp{Preferences: profile.Preferences}, err)
	}
}

func updateMyPreferencesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.MyPreferencesPatchReq
		if err := decodeContractJSON(w, r, &req); err != nil {
			writeContractError(w, http.StatusBadRequest, "INVALID_REQUEST", err.Error())
			return
		}
		if req.Preferences == nil {
			writeContractError(w, http.StatusBadRequest, "INVALID_REQUEST", "preferences is required")
			return
		}
		var userID string
		if !setCurrentUserID(r, &userID) {
			writeContractError(w, http.StatusUnauthorized, "UNAUTHORIZED", "unauthorized")
			return
		}
		resp, err := svcCtx.ProfileStore.UpdateCtx(r.Context(), userID, store.ProfilePatch{Preferences: req.Preferences})
		writeLogicResult(r, w, resp, err)
	}
}

func adminGetProfileHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ProfileReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		resp, err := svcCtx.ProfileStore.GetOrCreateCtx(r.Context(), req.UserId, req.UserId)
		writeLogicResult(r, w, &resp, err)
	}
}

func adminUpdateProfileHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ProfilePatchReq
		if err := httpx.ParsePath(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		var body types.MyProfilePatchReq
		if err := decodeContractJSON(w, r, &body); err != nil {
			writeContractError(w, http.StatusBadRequest, "INVALID_REQUEST", err.Error())
			return
		}
		resp, err := svcCtx.ProfileStore.UpdateCtx(r.Context(), req.UserId, store.ProfilePatch{
			DisplayName: body.DisplayName, Bio: body.Bio, AvatarObject: body.AvatarObject,
			Preferences: body.Preferences,
		})
		writeLogicResult(r, w, &resp, err)
	}
}

func adminGetPreferencesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ProfileReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		profile, err := svcCtx.ProfileStore.GetOrCreateCtx(r.Context(), req.UserId, req.UserId)
		writeLogicResult(r, w, &types.PreferencesResp{Preferences: profile.Preferences}, err)
	}
}

func adminUpdatePreferencesHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ProfilePatchReq
		if err := httpx.ParsePath(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		var body types.MyPreferencesPatchReq
		if err := decodeContractJSON(w, r, &body); err != nil {
			writeContractError(w, http.StatusBadRequest, "INVALID_REQUEST", err.Error())
			return
		}
		if body.Preferences == nil {
			writeContractError(w, http.StatusBadRequest, "INVALID_REQUEST", "preferences is required")
			return
		}
		resp, err := svcCtx.ProfileStore.UpdateCtx(r.Context(), req.UserId, store.ProfilePatch{Preferences: body.Preferences})
		writeLogicResult(r, w, &resp, err)
	}
}

func adminGetStatsHandler(svcCtx *svc.ServiceContext) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var req types.ProfileReq
		if err := httpx.Parse(r, &req); err != nil {
			httpx.ErrorCtx(r.Context(), w, err)
			return
		}
		profile, err := svcCtx.ProfileStore.GetOrCreateCtx(r.Context(), req.UserId, req.UserId)
		writeLogicResult(r, w, &profile.Stats, err)
	}
}

func setCurrentUserID(r *http.Request, target *string) bool {
	user, ok := authctx.FromContext(r.Context())
	if !ok || user == nil || user.UserID <= 0 {
		return false
	}
	*target = strconv.FormatInt(user.UserID, 10)
	return true
}

func writeLogicResult(r *http.Request, w http.ResponseWriter, value any, err error) {
	if err != nil {
		httpx.ErrorCtx(r.Context(), w, err)
		return
	}
	httpx.OkJsonCtx(r.Context(), w, value)
}

func decodeContractJSON(w http.ResponseWriter, r *http.Request, target any) error {
	const maxContractBodyBytes = 1 << 20
	if r.Body == nil {
		return errors.New("JSON request body is required")
	}
	r.Body = http.MaxBytesReader(w, r.Body, maxContractBodyBytes)
	decoder := json.NewDecoder(r.Body)
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(target); err != nil {
		return fmt.Errorf("invalid JSON request body: %w", err)
	}
	var trailing any
	if err := decoder.Decode(&trailing); !errors.Is(err, io.EOF) {
		return errors.New("JSON request body must contain exactly one document")
	}
	return nil
}

func writeContractError(w http.ResponseWriter, status int, code, message string) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(map[string]string{"code": code, "message": message})
}
