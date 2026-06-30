package logic

import (
	"strings"

	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/repository"
	"ojos-problem-service/internal/types"
)

func convertProblem(p repository.Problem) types.ProblemItem {
	return types.ProblemItem{
		Id:             p.ID,
		Slug:           p.Slug,
		Title:          p.Title,
		Statement:      p.Statement,
		ProblemType:    p.ProblemType,
		Visibility:     p.Visibility,
		ManifestSha256: p.ManifestSha256,
		SourceFormat:   p.SourceFormat,
		Status:         p.Status,
		Difficulty:     p.Difficulty,
		Tags:           strings.Join(p.Tags, ","),
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

func convertSamples(samples []packagefs.SampleRecord) []types.ProblemSample {
	items := make([]types.ProblemSample, 0, len(samples))
	for _, sample := range samples {
		items = append(items, types.ProblemSample{
			CaseNo: sample.CaseNo,
			Input:  sample.Input,
			Output: sample.Output,
		})
	}
	return items
}

func convertPackageSummary(summary packagefs.PackageSummary) types.PackageSummary {
	return types.PackageSummary{
		Schema:         summary.Schema,
		Slug:           summary.Slug,
		Title:          summary.Title,
		ProblemType:    summary.Type,
		Visibility:     summary.Visibility,
		Status:         summary.Status,
		SourceFormat:   summary.SourceFormat,
		ManifestSha256: summary.ManifestSha256,
		TotalCases:     summary.TotalCases,
		TotalScore:     summary.TotalScore,
		SampleCount:    summary.SampleCount,
		FileCount:      summary.FileCount,
		SizeBytes:      summary.SizeBytes,
		Limits:         convertPackageLimits(summary.Limits),
		Runner:         convertPackageComponent(summary.Runner),
		Checker:        convertPackageComponent(summary.Checker),
		Scorer:         convertPackageComponent(summary.Scorer),
	}
}

func convertPackageLimits(limits packagefs.PackageLimitsSummary) types.PackageLimits {
	languages := make([]types.PackageLanguageLimit, 0, len(limits.Languages))
	for _, language := range limits.Languages {
		languages = append(languages, types.PackageLanguageLimit{
			Language:      language.Language,
			TimeLimitMs:   language.TimeMs,
			MemoryLimitMb: language.MemoryMb,
		})
	}

	return types.PackageLimits{
		DefaultTimeLimitMs:   limits.DefaultTimeMs,
		DefaultMemoryLimitMb: limits.DefaultMemoryMb,
		Languages:            languages,
	}
}

func convertPackageComponent(component packagefs.PackageComponentSummary) types.PackageComponent {
	return types.PackageComponent{
		Type:       component.Type,
		Name:       component.Name,
		ConfigPath: component.ConfigPath,
	}
}

func convertPackageValidation(validation packagefs.PackageValidationResult) types.PackageValidationResult {
	return types.PackageValidationResult{
		Valid:    validation.Valid,
		Errors:   convertPackageIssues(validation.Errors),
		Warnings: convertPackageIssues(validation.Warnings),
	}
}

func convertPackageIssues(issues []packagefs.PackageValidationIssue) []types.PackageValidationIssue {
	items := make([]types.PackageValidationIssue, 0, len(issues))
	for _, issue := range issues {
		items = append(items, types.PackageValidationIssue{
			Level:   issue.Level,
			Code:    issue.Code,
			Message: issue.Message,
			Path:    issue.Path,
			CaseNo:  issue.CaseNo,
		})
	}
	return items
}
