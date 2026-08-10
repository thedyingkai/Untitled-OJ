// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"path/filepath"
	"strconv"
	"strings"

	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/packagemutation"
	"ojos-problem-service/internal/repository"
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

	problemID, err := l.svcCtx.Repo.ReserveProblemID(l.ctx)
	if err != nil {
		return nil, err
	}
	if problemNo == "" {
		problemNo = "P" + strconv.FormatInt(problemID, 10)
	}
	createArg := repository.CreateProblemArg{
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
	}

	pkg, err := packagemutation.RunCreate(
		l.ctx,
		l.svcCtx.Repo,
		l.svcCtx.Config.Storage,
		packagefs.CreateProblemArgs{
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
		},
		func(txRepo *repository.Repository, result *packagefs.CreateProblemResult) error {
			if err := txRepo.InsertProblemWithID(l.ctx, problemID, createArg); err != nil {
				return err
			}
			if err := txRepo.UpdateProblemPackage(l.ctx, problemID, filepath.Base(result.PackageDir), result.PackageDir, result.ManifestPath, result.ManifestSha256); err != nil {
				return err
			}
			if err := txRepo.UpsertProblemFiles(l.ctx, problemID, result.Files); err != nil {
				return err
			}
			return txRepo.BindProblemOwner(l.ctx, user.UserID, problemID)
		},
	)
	if err != nil {
		return nil, err
	}
	slug := filepath.Base(pkg.PackageDir)

	return &types.CreateProblemResp{
		ProblemId: problemID,
		ProblemNo: problemNo,
		Slug:      slug,
	}, nil
}
