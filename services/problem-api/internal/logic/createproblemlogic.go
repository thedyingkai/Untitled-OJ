// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-problem-api/internal/packagefs"
	"ojos-problem-api/internal/repository"
	"ojos-problem-api/internal/svc"
	"ojos-problem-api/internal/types"
	"ojos-shared/security/authctx"
	"ojos-shared/security/permission"

	"github.com/zeromicro/go-zero/core/logx"
)

type CreateProblemLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewCreateProblemLogic(ctx context.Context, svcCtx *svc.ServiceContext) *CreateProblemLogic {
	return &CreateProblemLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *CreateProblemLogic) CreateProblem(req *types.CreateProblemReq) (resp *types.CreateProblemResp, err error) {
	user, ok := authctx.FromContext(l.ctx)
	if !ok || user == nil || user.UserID <= 0 {
		return nil, errors.New("unauthorized")
	}

	if err := permission.RequireUserPermission(
		l.ctx,
		l.svcCtx.DB,
		user.UserID,
		"problem.create",
		permission.SystemScope(),
	); err != nil {
		return nil, err
	}

	title := strings.TrimSpace(req.Title)
	if title == "" {
		return nil, errors.New("empty title")
	}

	problemType := strings.TrimSpace(req.ProblemType)
	if problemType == "" {
		problemType = "traditional"
	}

	visibility := strings.TrimSpace(req.Visibility)
	if visibility == "" {
		visibility = "private"
	}

	timeLimitMs := req.TimeLimitMs
	if timeLimitMs <= 0 {
		timeLimitMs = 1000
	}

	memoryLimitMb := req.MemoryLimitMb
	if memoryLimitMb <= 0 {
		memoryLimitMb = 256
	}

	problemID, err := l.svcCtx.Repo.InsertProblem(
		l.ctx,
		repository.CreateProblemArg{
			Title:         title,
			Statement:     strings.TrimSpace(req.Statement),
			ProblemType:   problemType,
			Visibility:    visibility,
			TimeLimitMs:   timeLimitMs,
			MemoryLimitMb: memoryLimitMb,
			CreatedBy:     user.UserID,
		},
	)
	if err != nil {
		return nil, err
	}

	pkg, err := packagefs.CreateInitialPackage(packagefs.CreateProblemArgs{
		Root:          l.svcCtx.Config.Storage.ProblemsRoot,
		ID:            problemID,
		Slug:          req.Slug,
		Title:         title,
		Statement:     req.Statement,
		ProblemType:   problemType,
		Visibility:    visibility,
		TimeLimitMs:   timeLimitMs,
		MemoryLimitMb: memoryLimitMb,
	})
	if err != nil {
		return nil, err
	}

	slug := packagefs.Slugify(req.Slug)
	if req.Slug == "" {
		slug = packagefs.Slugify(title)
	}
	slug = string(rune(0)) + slug
	slug = pkg.PackageDir[strings.LastIndex(pkg.PackageDir, "/")+1:]
	if strings.Contains(pkg.PackageDir, "\\") {
		slug = pkg.PackageDir[strings.LastIndex(pkg.PackageDir, "\\")+1:]
	}

	if err := l.svcCtx.Repo.UpdateProblemPackage(
		l.ctx,
		problemID,
		slug,
		pkg.PackageDir,
		pkg.ManifestPath,
		pkg.ManifestSha256,
	); err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.UpsertProblemFiles(l.ctx, problemID, pkg.Files); err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.BindProblemOwner(l.ctx, user.UserID, problemID); err != nil {
		return nil, err
	}

	return &types.CreateProblemResp{
		ProblemId:  problemID,
		Slug:       slug,
		PackageDir: pkg.PackageDir,
	}, nil
}
