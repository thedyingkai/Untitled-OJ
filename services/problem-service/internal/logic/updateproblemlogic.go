// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"
	"strings"

	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/packagemutation"
	"ojos-problem-service/internal/repository"
	"ojos-problem-service/internal/svc"
	"ojos-problem-service/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type UpdateProblemLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

type updateProblemPackageState struct {
	languageLimits              []repository.ProblemLanguageLimit
	shouldReplaceLanguageLimits bool
}

func NewUpdateProblemLogic(ctx context.Context, svcCtx *svc.ServiceContext) *UpdateProblemLogic {
	return &UpdateProblemLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *UpdateProblemLogic) UpdateProblem(req *types.UpdateProblemReq) (resp *types.GetProblemResp, err error) {
	if req.Id <= 0 {
		return nil, errors.New("invalid problem id")
	}

	if strings.TrimSpace(req.Title) == "" &&
		strings.TrimSpace(req.ProblemNo) == "" &&
		strings.TrimSpace(req.Statement) == "" &&
		strings.TrimSpace(req.Solution) == "" &&
		strings.TrimSpace(req.ProblemType) == "" &&
		strings.TrimSpace(req.Visibility) == "" &&
		strings.TrimSpace(req.Status) == "" &&
		strings.TrimSpace(req.Difficulty) == "" &&
		strings.TrimSpace(req.Tags) == "" &&
		req.LanguageLimits == nil &&
		!componentInputProvided(req.Runner) &&
		!componentInputProvided(req.Checker) &&
		!componentInputProvided(req.Validator) &&
		!componentInputProvided(req.Scorer) &&
		req.TimeLimitMs == 0 &&
		req.MemoryLimitMb == 0 {
		return nil, errors.New("empty update")
	}

	if _, err := requireProblemPermission(l.ctx, l.svcCtx, "problem.edit", req.Id); err != nil {
		return nil, err
	}

	problemNo, err := normalizeProblemNo(req.ProblemNo)
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
	shouldReplaceLanguageLimits := req.LanguageLimits != nil || req.TimeLimitMs > 0 || req.MemoryLimitMb > 0
	components, err := normalizeComponents(req.Runner, req.Checker, req.Validator, req.Scorer)
	if err != nil {
		return nil, err
	}

	_, err = packagemutation.RunExisting(
		l.ctx,
		l.svcCtx.Repo,
		l.svcCtx.Config.Storage,
		req.Id,
		func(p *repository.Problem, stagingDir string) (packagemutation.Change, error) {
			effectiveTimeLimitMs := p.TimeLimitMs
			if req.TimeLimitMs > 0 {
				effectiveTimeLimitMs = req.TimeLimitMs
			}
			effectiveMemoryLimitMb := p.MemoryLimitMb
			if req.MemoryLimitMb > 0 {
				effectiveMemoryLimitMb = req.MemoryLimitMb
			}
			var languageLimitsForRepo []repository.ProblemLanguageLimit
			var languageLimitsForPackage []packagefs.LanguageLimit
			if shouldReplaceLanguageLimits {
				var err error
				languageLimitsForRepo, languageLimitsForPackage, err = normalizeLanguageLimits(req.LanguageLimits, effectiveTimeLimitMs, effectiveMemoryLimitMb)
				if err != nil {
					return packagemutation.Change{}, err
				}
			}
			manifestSHA, files, err := packagefs.UpdateManifest(
				stagingDir,
				problemNo,
				strings.TrimSpace(req.Title),
				strings.TrimSpace(req.Statement),
				strings.TrimSpace(req.Solution),
				problemType,
				visibility,
				status,
				req.TimeLimitMs,
				req.MemoryLimitMb,
				languageLimitsForPackage,
				components,
			)
			return packagemutation.Change{
				Files:          files,
				ManifestSHA256: manifestSHA,
				Value: updateProblemPackageState{
					languageLimits:              languageLimitsForRepo,
					shouldReplaceLanguageLimits: shouldReplaceLanguageLimits,
				},
			}, err
		},
		func(txRepo *repository.Repository, _ *repository.Problem, change packagemutation.Change) error {
			state, ok := change.Value.(updateProblemPackageState)
			if !ok {
				return errors.New("invalid update package mutation state")
			}
			if err := txRepo.UpdateProblem(
				l.ctx,
				req.Id,
				problemNo,
				strings.TrimSpace(req.Title),
				strings.TrimSpace(req.Statement),
				strings.TrimSpace(req.Solution),
				problemType,
				visibility,
				status,
				difficulty,
				parseTagsForPut(req.Tags),
				req.TimeLimitMs,
				req.MemoryLimitMb,
				change.ManifestSHA256,
			); err != nil {
				return err
			}
			if state.shouldReplaceLanguageLimits {
				if err := txRepo.ReplaceProblemLanguageLimits(l.ctx, req.Id, state.languageLimits); err != nil {
					return err
				}
			}
			return txRepo.UpsertProblemFiles(l.ctx, req.Id, change.Files)
		},
	)
	if err != nil {
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
