// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/repository"
	problemstorage "ojos-problem-service/internal/storage"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"

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
	user, err := requireSystemProblemPermission(l.ctx, l.svcCtx, "problem.create")
	if err != nil {
		return nil, err
	}

	title := strings.TrimSpace(req.Title)
	if title == "" {
		return nil, errors.New("empty title")
	}

	if err := validateSlug(req.Slug); err != nil {
		return nil, err
	}
	problemNo, err := normalizeProblemNo(req.ProblemNo)
	if err != nil {
		return nil, err
	}

	problemType, err := normalizeProblemType(req.ProblemType)
	if err != nil {
		return nil, err
	}

	visibility, err := normalizeVisibility(req.Visibility)
	if err != nil {
		return nil, err
	}

	timeLimitMs := req.TimeLimitMs
	if timeLimitMs <= 0 {
		timeLimitMs = 1000
	}

	memoryLimitMb := req.MemoryLimitMb
	if memoryLimitMb <= 0 {
		memoryLimitMb = 256
	}

	if err := validateLimits(timeLimitMs, memoryLimitMb, false); err != nil {
		return nil, err
	}
	languageLimits, packageLanguageLimits, err := normalizeLanguageLimits(req.LanguageLimits, timeLimitMs, memoryLimitMb)
	if err != nil {
		return nil, err
	}
	components, err := normalizeComponents(req.Runner, req.Checker, req.Validator, req.Scorer)
	if err != nil {
		return nil, err
	}

	difficulty, err := normalizeDifficulty(req.Difficulty)
	if err != nil {
		return nil, err
	}

	problemID, problemNo, err := l.svcCtx.Repo.InsertProblem(
		l.ctx,
		repository.CreateProblemArg{
			ProblemNo:       problemNo,
			Title:           title,
			Statement:       strings.TrimSpace(req.Statement),
			StatementFormat: packagefs.ContentFormatMarkdownLatex,
			Solution:        strings.TrimSpace(req.Solution),
			SolutionFormat:  packagefs.ContentFormatMarkdownLatex,
			ProblemType:     problemType,
			Visibility:      visibility,
			Difficulty:      difficulty,
			Tags:            parseTags(req.Tags),
			TimeLimitMs:     timeLimitMs,
			MemoryLimitMb:   memoryLimitMb,
			LanguageLimits:  languageLimits,
			CreatedBy:       user.UserID,
		},
	)
	if err != nil {
		return nil, err
	}

	pkg, err := packagefs.CreateInitialPackage(packagefs.CreateProblemArgs{
		Root:           l.svcCtx.Config.Storage.ProblemsRoot,
		ID:             problemID,
		ProblemNo:      problemNo,
		Slug:           req.Slug,
		Title:          title,
		Statement:      req.Statement,
		Solution:       req.Solution,
		ProblemType:    problemType,
		Visibility:     visibility,
		TimeLimitMs:    timeLimitMs,
		MemoryLimitMb:  memoryLimitMb,
		LanguageLimits: packageLanguageLimits,
		Components:     components,
	})
	if err != nil {
		return nil, err
	}

	slug := packagefs.Slugify(req.Slug)
	if req.Slug == "" {
		slug = packagefs.Slugify(title)
	}
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

	files, err := problemstorage.SyncProblemFiles(l.ctx, l.svcCtx.Config.Storage, problemID, pkg.Files)
	if err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.UpsertProblemFiles(l.ctx, problemID, files); err != nil {
		return nil, err
	}

	if err := l.svcCtx.Repo.BindProblemOwner(l.ctx, user.UserID, problemID); err != nil {
		return nil, err
	}

	return &types.CreateProblemResp{
		ProblemId: problemID,
		ProblemNo: problemNo,
		Slug:      slug,
	}, nil
}
