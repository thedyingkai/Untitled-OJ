// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strings"
	"time"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
	"ojos-shared/security/authctx"
	sharedperm "ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/logx"
)

type ListSubmissionsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewListSubmissionsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *ListSubmissionsLogic {
	return &ListSubmissionsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *ListSubmissionsLogic) ListSubmissions(req *types.ListSubmissionsReq) (resp *types.ListSubmissionsResp, err error) {
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	status := strings.TrimSpace(req.Status)
	if status != "" && !isValidJudgeStatus(status) {
		return nil, errors.New("invalid status")
	}

	language := ""
	if strings.TrimSpace(req.Language) != "" {
		language, err = validateEnabledLanguage(l.svcCtx, req.Language)
		if err != nil {
			return nil, err
		}
	}

	createdFrom, err := parseSubmissionTime(req.CreatedFrom, false)
	if err != nil {
		return nil, err
	}
	createdTo, err := parseSubmissionTime(req.CreatedTo, true)
	if err != nil {
		return nil, err
	}

	permissions := l.svcCtx.ActivePermissionChecker()
	if permissions == nil {
		return nil, errors.New("permission checker is not configured")
	}

	canViewAll, err := permissions.HasUserPermission(
		l.ctx,
		user.UserID,
		"submission.view.all",
		sharedperm.SystemScope(),
	)
	if err != nil {
		return nil, err
	}

	canViewProblem := false
	if req.ProblemId > 0 && !canViewAll {
		canViewProblem, err = permissions.HasUserPermission(
			l.ctx,
			user.UserID,
			"problem.manage.data",
			sharedperm.Scope{Type: "problem", ID: req.ProblemId},
		)
		if err != nil {
			return nil, err
		}
	}

	restrictToUserID := int64(0)
	if !canViewAll && !canViewProblem {
		restrictToUserID = user.UserID
		if req.UserId > 0 && req.UserId != user.UserID {
			return nil, sharedperm.ErrForbidden
		}
	}

	submissions, total, err := l.svcCtx.Repo.ListSubmissions(l.ctx, repository.ListSubmissionsFilter{
		Page:             req.Page,
		PageSize:         req.PageSize,
		Status:           status,
		ProblemID:        req.ProblemId,
		UserID:           req.UserId,
		Language:         language,
		CreatedFrom:      createdFrom,
		CreatedTo:        createdTo,
		RestrictToUserID: restrictToUserID,
	})
	if err != nil {
		return nil, err
	}

	items := make([]types.SubmissionItem, 0, len(submissions))
	for _, submission := range submissions {
		items = append(items, convertSubmissionItem(submission))
	}

	return &types.ListSubmissionsResp{
		Submissions: items,
		Total:       total,
	}, nil
}

func parseSubmissionTime(value string, endOfDay bool) (*time.Time, error) {
	value = strings.TrimSpace(value)
	if value == "" {
		return nil, nil
	}

	if parsed, err := time.Parse(time.RFC3339, value); err == nil {
		return &parsed, nil
	}

	parsed, err := time.Parse("2006-01-02", value)
	if err != nil {
		return nil, errors.New("invalid time range")
	}
	if endOfDay {
		parsed = parsed.Add(24*time.Hour - time.Nanosecond)
	}
	return &parsed, nil
}

func isValidJudgeStatus(status string) bool {
	switch status {
	case "PENDING",
		"JUDGING",
		"ACCEPTED",
		"WRONG_ANSWER",
		"COMPILE_ERROR",
		"RUNTIME_ERROR",
		"TIME_LIMIT_EXCEEDED",
		"MEMORY_LIMIT_EXCEEDED",
		"OUTPUT_LIMIT_EXCEEDED",
		"SYSTEM_ERROR",
		"CANCELLED",
		"UNSUPPORTED_LANGUAGE":
		return true
	default:
		return false
	}
}
