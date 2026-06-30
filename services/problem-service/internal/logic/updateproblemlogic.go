// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"
	"ojos-shared/security/authctx"
	"ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/logx"
)

type UpdateProblemLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewUpdateProblemLogic(ctx context.Context, svcCtx *svc.ServiceContext) *UpdateProblemLogic {
	return &UpdateProblemLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *UpdateProblemLogic) UpdateProblem(req *types.UpdateProblemReq) (resp *types.GetProblemResp, err error) {
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	if req.Id <= 0 {
		return nil, errors.New("invalid problem id")
	}

	if strings.TrimSpace(req.Title) == "" &&
		strings.TrimSpace(req.Statement) == "" &&
		strings.TrimSpace(req.ProblemType) == "" &&
		strings.TrimSpace(req.Visibility) == "" &&
		strings.TrimSpace(req.Status) == "" &&
		strings.TrimSpace(req.Difficulty) == "" &&
		strings.TrimSpace(req.Tags) == "" &&
		req.TimeLimitMs == 0 &&
		req.MemoryLimitMb == 0 {
		return nil, errors.New("empty update")
	}

	if err := permission.RequireUserPermission(
		l.ctx,
		l.svcCtx.DB,
		user.UserID,
		"problem.edit",
		permission.Scope{Type: "problem", ID: req.Id},
	); err != nil {
		return nil, err
	}

	p, err := l.svcCtx.Repo.GetProblem(l.ctx, req.Id)
	if err != nil {
		return nil, err
	}

	problemType := ""
	if strings.TrimSpace(req.ProblemType) != "" {
		problemType, err = normalizeProblemType(req.ProblemType)
		if err != nil {
			return nil, err
		}
	}

	visibility := ""
	if strings.TrimSpace(req.Visibility) != "" {
		visibility, err = normalizeVisibility(req.Visibility)
		if err != nil {
			return nil, err
		}
	}

	status := ""
	if strings.TrimSpace(req.Status) != "" {
		status, err = normalizeStatus(req.Status)
		if err != nil {
			return nil, err
		}
	}

	var difficulty string
	if strings.TrimSpace(req.Difficulty) != "" {
		difficulty, err = normalizeDifficulty(req.Difficulty)
		if err != nil {
			return nil, err
		}
	}

	if err := validateLimits(req.TimeLimitMs, req.MemoryLimitMb, true); err != nil {
		return nil, err
	}

	manifestSha, files, err := packagefs.UpdateManifest(
		p.PackageDir,
		strings.TrimSpace(req.Title),
		strings.TrimSpace(req.Statement),
		problemType,
		visibility,
		status,
		req.TimeLimitMs,
		req.MemoryLimitMb,
	)
	if err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.UpdateProblem(
		l.ctx,
		req.Id,
		strings.TrimSpace(req.Title),
		strings.TrimSpace(req.Statement),
		problemType,
		visibility,
		status,
		difficulty,
		parseTagsForPut(req.Tags),
		req.TimeLimitMs,
		req.MemoryLimitMb,
		manifestSha,
	); err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.UpsertProblemFiles(l.ctx, req.Id, files); err != nil {
		return nil, err
	}

	updated, err := l.svcCtx.Repo.GetProblem(l.ctx, req.Id)
	if err != nil {
		return nil, err
	}

	return &types.GetProblemResp{
		Problem: convertProblem(*updated),
	}, nil
}
