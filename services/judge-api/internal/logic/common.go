package logic

import (
	"encoding/json"
	"errors"
	"io"
	"os"
	"time"

	"ojos-judge-api/internal/repository"
	"ojos-judge-api/internal/types"
)

const (
	maxResultJSONBytes     = int64(8 * 1024 * 1024)
	defaultDebugLogMaxByte = 32 * 1024
	maxDebugLogMaxByte     = 128 * 1024
)

type ResultFile struct {
	SubmissionID int64            `json:"submission_id"`
	Status       string           `json:"status"`
	Score        int              `json:"score"`
	TimeMS       int              `json:"time_ms"`
	MemoryKB     int              `json:"memory_kb"`
	Message      string           `json:"message"`
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
		CodeSha256:   s.CodeSha256,
		CreatedAt:    s.CreatedAt.UTC().Format(time.RFC3339Nano),
		UpdatedAt:    s.UpdatedAt.UTC().Format(time.RFC3339Nano),
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

func convertSubmissionItem(s repository.SubmissionView) types.SubmissionItem {
	resp := types.SubmissionItem{
		Id:           s.ID,
		ProblemId:    s.ProblemID,
		UserId:       s.UserID,
		Language:     s.Language,
		Status:       s.Status,
		Score:        s.Score,
		TimeMs:       s.TimeMS,
		MemoryKb:     s.MemoryKB,
		Message:      s.Message,
		CodeSha256:   s.CodeSha256,
		CreatedAt:    s.CreatedAt.UTC().Format(time.RFC3339Nano),
		UpdatedAt:    s.UpdatedAt.UTC().Format(time.RFC3339Nano),
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

	data, err := readLimitedFile(resultPath, maxResultJSONBytes)
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
			CaseNo:   c.CaseNo,
			Status:   c.Status,
			Score:    c.Score,
			TimeMs:   c.TimeMS,
			MemoryKb: c.MemoryKB,
			Message:  c.Message,
		})
	}

	return items, nil
}

func readResultFile(resultPath string) (*ResultFile, error) {
	if resultPath == "" {
		return &ResultFile{}, nil
	}

	data, err := readLimitedFile(resultPath, maxResultJSONBytes)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return &ResultFile{}, nil
		}
		return nil, err
	}

	var result ResultFile
	if err := json.Unmarshal(data, &result); err != nil {
		return nil, err
	}
	return &result, nil
}

func readLimitedFile(path string, maxBytes int64) ([]byte, error) {
	stat, err := os.Stat(path)
	if err != nil {
		return nil, err
	}
	if stat.Size() > maxBytes {
		return nil, errors.New("file exceeds read limit")
	}
	return os.ReadFile(path)
}

func readTruncatedText(path string, maxBytes int) (string, bool, error) {
	if path == "" {
		return "", false, nil
	}
	if maxBytes <= 0 {
		maxBytes = defaultDebugLogMaxByte
	}
	if maxBytes > maxDebugLogMaxByte {
		maxBytes = maxDebugLogMaxByte
	}

	file, err := os.Open(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return "", false, nil
		}
		return "", false, err
	}
	defer file.Close()

	data, err := io.ReadAll(io.LimitReader(file, int64(maxBytes)+1))
	if err != nil {
		return "", false, err
	}
	truncated := len(data) > maxBytes
	if truncated {
		data = data[:maxBytes]
	}
	return string(data), truncated, nil
}
