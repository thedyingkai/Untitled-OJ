package logic

import (
	"ojos-problem-api/internal/packagefs"
	"ojos-problem-api/internal/repository"
	"ojos-problem-api/internal/types"
)

func convertProblem(p repository.Problem) types.ProblemItem {
	return types.ProblemItem{
		Id:             p.ID,
		Slug:           p.Slug,
		Title:          p.Title,
		Statement:      p.Statement,
		ProblemType:    p.ProblemType,
		Visibility:     p.Visibility,
		PackageDir:     p.PackageDir,
		ManifestPath:   p.ManifestPath,
		ManifestSha256: p.ManifestSha256,
		SourceFormat:   p.SourceFormat,
		Status:         p.Status,
		TimeLimitMs:    p.TimeLimitMs,
		MemoryLimitMb:  p.MemoryLimitMb,
		CreatedBy:      p.CreatedBy,
		CreatedAt:      p.CreatedAt.Format("2006-01-02T15:04:05Z07:00"),
		UpdatedAt:      p.UpdatedAt.Format("2006-01-02T15:04:05Z07:00"),
	}
}

func convertCase(c packagefs.CaseRecord) types.TestCaseItem {
	return types.TestCaseItem{
		No:            c.No,
		Input:         c.Input,
		Answer:        c.Answer,
		Score:         c.Score,
		Group:         c.Group,
		Sample:        c.Sample,
		Hidden:        c.Hidden,
		TimeLimitMs:   c.TimeLimitMs,
		MemoryLimitMb: c.MemoryLimitMb,
	}
}
