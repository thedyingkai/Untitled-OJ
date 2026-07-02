package logic

import (
	"fmt"
	"strings"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
)

const defaultMaxCodeBytes int64 = 256 * 1024

func configuredLanguages(svcCtx *svc.ServiceContext) []config.LanguageConfig {
	return svcCtx.Config.Languages.Items
}

func maxCodeBytes(svcCtx *svc.ServiceContext) int64 {
	if svcCtx.Config.Submission.MaxCodeBytes <= 0 {
		return defaultMaxCodeBytes
	}
	return svcCtx.Config.Submission.MaxCodeBytes
}

func normalizeLanguageID(language string) string {
	return strings.ToLower(strings.TrimSpace(language))
}

func validateEnabledLanguage(svcCtx *svc.ServiceContext, language string) (string, error) {
	id := normalizeLanguageID(language)
	if id == "" {
		return "", fmt.Errorf("language is required")
	}

	for _, item := range configuredLanguages(svcCtx) {
		itemID := normalizeLanguageID(item.Id)
		if itemID != id {
			continue
		}
		if !item.Enabled {
			return "", fmt.Errorf("language is disabled: %s", id)
		}
		return itemID, nil
	}

	return "", fmt.Errorf("unsupported language: %s", id)
}

func sourceFileForLanguage(svcCtx *svc.ServiceContext, language string) string {
	id := normalizeLanguageID(language)
	for _, item := range configuredLanguages(svcCtx) {
		if normalizeLanguageID(item.Id) == id {
			return strings.TrimSpace(item.SourceFile)
		}
	}
	return ""
}

func convertLanguages(svcCtx *svc.ServiceContext) []types.JudgeLanguage {
	languages := configuredLanguages(svcCtx)
	items := make([]types.JudgeLanguage, 0, len(languages))
	for _, language := range languages {
		items = append(items, types.JudgeLanguage{
			Id:          language.Id,
			DisplayName: language.DisplayName,
			Version:     language.Version,
			Enabled:     language.Enabled,
			SourceFile:  strings.TrimSpace(language.SourceFile),
		})
	}
	return items
}
