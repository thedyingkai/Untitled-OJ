package logic

import (
	"fmt"
	"strings"

	"ojos-judge-api/internal/config"
	"ojos-judge-api/internal/svc"
	"ojos-judge-api/internal/types"
)

const defaultMaxCodeBytes int64 = 256 * 1024

var defaultLanguages = []config.LanguageConfig{
	{Id: "cpp17", DisplayName: "C++17", Version: "GCC C++17", Enabled: true},
	{Id: "c11", DisplayName: "C11", Version: "GCC C11", Enabled: true},
	{Id: "python3", DisplayName: "Python 3", Version: "CPython 3", Enabled: true},
	{Id: "java17", DisplayName: "Java 17", Version: "OpenJDK 17", Enabled: true},
}

func configuredLanguages(svcCtx *svc.ServiceContext) []config.LanguageConfig {
	if len(svcCtx.Config.Languages.Items) == 0 {
		return defaultLanguages
	}
	return svcCtx.Config.Languages.Items
}

func maxCodeBytes(svcCtx *svc.ServiceContext) int64 {
	if svcCtx.Config.Submission.MaxCodeBytes <= 0 {
		return defaultMaxCodeBytes
	}
	return svcCtx.Config.Submission.MaxCodeBytes
}

func normalizeLanguageID(language string) string {
	switch strings.ToLower(strings.TrimSpace(language)) {
	case "cpp", "c++", "cpp17":
		return "cpp17"
	case "c", "c11":
		return "c11"
	case "py", "py3", "python", "python3":
		return "python3"
	case "java", "java17":
		return "java17"
	default:
		return strings.ToLower(strings.TrimSpace(language))
	}
}

func validateEnabledLanguage(svcCtx *svc.ServiceContext, language string) (string, error) {
	id := normalizeLanguageID(language)
	if id == "" {
		return "", fmt.Errorf("language is required")
	}

	for _, item := range configuredLanguages(svcCtx) {
		if item.Id != id {
			continue
		}
		if !item.Enabled {
			return "", fmt.Errorf("language is disabled: %s", id)
		}
		return id, nil
	}

	return "", fmt.Errorf("unsupported language: %s", id)
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
		})
	}
	return items
}
