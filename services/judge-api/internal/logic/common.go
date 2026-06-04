package logic

import (
	"encoding/json"
	"errors"
	"os"
	"time"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/types"
)

type ResultFile struct {
	SubmissionID int64            `json:"submission_id"`
	Status       string           `json:"status"`
	Score        int              `json:"score"`
	TimeMS       int              `json:"time_ms"`
	MemoryKB     int              `json:"memory_kb"`
	Cases        []ResultCaseItem `json:"cases"`
}

type ResultCaseItem struct {
	CaseNo         int    `json:"case_no"`
	Status         string `json:"status"`
	Score          int    `json:"score"`
	TimeMS         int    `json:"time_ms"`
	MemoryKB       int    `json:"memory_kb"`
	StdoutPath     string `json:"stdout_path,omitempty"`
	StderrPath     string `json:"stderr_path,omitempty"`
	CheckerLogPath string `json:"checker_log_path,omitempty"`
	Message        string `json:"message,omitempty"`
}

func convertSubmission(s *repository.SubmissionView) types.GetSubmissionResp {
	resp := types.GetSubmissionResp{
		Id:           s.ID,
		ProblemId:    s.ProblemID,
		UserId:       s.UserID,
		Language:     s.Language,
		Status:       s.Status,
		Score:        s.Score,
		TimeMs:       s.TimeMS,
		MemoryKb:     s.MemoryKB,
		Message:      s.Message,
		CodePath:     s.CodePath,
		CodeSha256:   s.CodeSha256,
		ResultPath:   s.ResultPath,
		CancelReason: s.CancelReason,
	}

	if s.JudgedAt != nil {
		resp.JudgedAt = s.JudgedAt.UTC().Format(time.RFC3339Nano)
	}

	if s.CancelledAt != nil {
		resp.CancelledAt = s.CancelledAt.UTC().Format(time.RFC3339Nano)
	}

	return resp
}

func readResultCases(resultPath string) ([]types.SubmissionCaseItem, error) {
	if resultPath == "" {
		return []types.SubmissionCaseItem{}, nil
	}

	data, err := os.ReadFile(resultPath)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return []types.SubmissionCaseItem{}, nil
		}
		return nil, err
	}

	var result ResultFile
	if err := json.Unmarshal(data, &result); err != nil {
		return nil, err
	}

	items := make([]types.SubmissionCaseItem, 0, len(result.Cases))
	for _, c := range result.Cases {
		items = append(items, types.SubmissionCaseItem{
			CaseNo:         c.CaseNo,
			Status:         c.Status,
			Score:          c.Score,
			TimeMs:         c.TimeMS,
			MemoryKb:       c.MemoryKB,
			StdoutPath:     c.StdoutPath,
			StderrPath:     c.StderrPath,
			CheckerLogPath: c.CheckerLogPath,
			Message:        c.Message,
		})
	}

	return items, nil
}
