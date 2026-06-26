// Code scaffolded by goctl. Safe to edit.
// goctl 1.10.1

package logic

import (
	"context"
	"errors"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"

	"github.com/zeromicro/go-zero/core/logx"
)

type GetSubmissionDebugLogsLogic struct {
	logx.Logger
	ctx    context.Context
	svcCtx *svc.ServiceContext
}

func NewGetSubmissionDebugLogsLogic(ctx context.Context, svcCtx *svc.ServiceContext) *GetSubmissionDebugLogsLogic {
	return &GetSubmissionDebugLogsLogic{
		Logger: logx.WithContext(ctx),
		ctx:    ctx,
		svcCtx: svcCtx,
	}
}

func (l *GetSubmissionDebugLogsLogic) GetSubmissionDebugLogs(req *types.GetSubmissionDebugLogsReq) (resp *types.SubmissionDebugLogsResp, err error) {
	if req.Id <= 0 {
		return nil, errors.New("invalid submission id")
	}

	submission, err := l.svcCtx.Repo.GetSubmission(l.ctx, req.Id)
	if err != nil {
		if errors.Is(err, repository.ErrSubmissionNotFound) {
			return nil, errors.New("submission not found")
		}
		return nil, err
	}

	if err := requireSubmissionDebugPermission(l.ctx, l.svcCtx, submission); err != nil {
		return nil, err
	}

	result, err := readResultFile(submission.ResultPath)
	if err != nil {
		return nil, err
	}

	target, ok := selectResultCase(result.Cases, req.CaseNo)
	if !ok {
		return nil, errors.New("case result not found")
	}

	maxBytes := req.MaxBytes
	if maxBytes <= 0 {
		maxBytes = defaultDebugLogMaxByte
	}
	if maxBytes > maxDebugLogMaxByte {
		maxBytes = maxDebugLogMaxByte
	}

	stdout, stdoutTruncated, err := readTruncatedText(target.StdoutPath, maxBytes)
	if err != nil {
		return nil, err
	}
	stderr, stderrTruncated, err := readTruncatedText(target.StderrPath, maxBytes)
	if err != nil {
		return nil, err
	}
	checkerLog, checkerTruncated, err := readTruncatedText(target.CheckerLogPath, maxBytes)
	if err != nil {
		return nil, err
	}

	return &types.SubmissionDebugLogsResp{
		CaseNo:     target.CaseNo,
		Stdout:     stdout,
		Stderr:     stderr,
		CheckerLog: checkerLog,
		Truncated:  stdoutTruncated || stderrTruncated || checkerTruncated,
		MaxBytes:   maxBytes,
	}, nil
}

func selectResultCase(cases []ResultCaseItem, caseNo int) (ResultCaseItem, bool) {
	if len(cases) == 0 {
		return ResultCaseItem{}, false
	}
	if caseNo <= 0 {
		return cases[0], true
	}
	for _, item := range cases {
		if item.CaseNo == caseNo {
			return item, true
		}
	}
	return ResultCaseItem{}, false
}
