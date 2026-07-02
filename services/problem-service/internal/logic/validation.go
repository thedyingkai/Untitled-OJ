package logic

import (
	"fmt"
	"regexp"
	"sort"
	"strings"

	"ojos-problem-service/internal/packagefs"
	"ojos-problem-service/internal/repository"
	"ojos-problem-service/internal/types"
)

var slugPattern = regexp.MustCompile(`^[a-z0-9]+(?:-[a-z0-9]+)*$`)
var problemNoPattern = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$`)
var languagePattern = regexp.MustCompile(`^[a-z0-9][a-z0-9._+-]{0,31}$`)

func validateSlug(slug string) error {
	slug = strings.TrimSpace(slug)
	if slug == "" {
		return nil
	}
	if !slugPattern.MatchString(slug) {
		return fmt.Errorf("invalid slug: use lowercase letters, numbers and hyphen")
	}
	return nil
}

func normalizeProblemNo(problemNo string) (string, error) {
	problemNo = strings.TrimSpace(problemNo)
	if problemNo == "" {
		return "", nil
	}
	if !problemNoPattern.MatchString(problemNo) {
		return "", fmt.Errorf("invalid problem_no: use 1..32 letters, numbers, dot, underscore or hyphen")
	}
	return problemNo, nil
}

func normalizeProblemType(value string) (string, error) {
	value = strings.ToLower(strings.TrimSpace(value))
	if value == "" {
		return "traditional", nil
	}

	switch value {
	case "traditional", "interactive", "communication", "output_only", "heuristic":
		return value, nil
	default:
		return "", fmt.Errorf("invalid problem_type: %s", value)
	}
}

func normalizeVisibility(value string) (string, error) {
	value = strings.ToLower(strings.TrimSpace(value))
	if value == "" {
		return "private", nil
	}

	switch value {
	case "private", "public":
		return value, nil
	default:
		return "", fmt.Errorf("invalid visibility: %s", value)
	}
}

func normalizeStatus(value string) (string, error) {
	value = strings.ToLower(strings.TrimSpace(value))
	if value == "" {
		return "", nil
	}

	switch value {
	case "draft", "ready", "published", "archived":
		return value, nil
	default:
		return "", fmt.Errorf("invalid status: %s", value)
	}
}

func normalizeDifficulty(value string) (string, error) {
	value = strings.ToLower(strings.TrimSpace(value))
	if value == "" {
		return "medium", nil
	}

	switch value {
	case "easy", "medium", "hard":
		return value, nil
	default:
		return "", fmt.Errorf("invalid difficulty: %s", value)
	}
}

func normalizeLanguageLimits(
	raw []types.ProblemLanguageLimit,
	defaultTimeMs int,
	defaultMemoryMb int,
) ([]repository.ProblemLanguageLimit, []packagefs.LanguageLimit, error) {
	if defaultTimeMs <= 0 {
		defaultTimeMs = 1000
	}
	if defaultMemoryMb <= 0 {
		defaultMemoryMb = 256
	}

	overrides := make([]packagefs.LanguageLimit, 0, len(raw))
	for _, item := range raw {
		language := strings.ToLower(strings.TrimSpace(item.Language))
		if language == "" {
			return nil, nil, fmt.Errorf("language is required in language_limits")
		}
		if !languagePattern.MatchString(language) {
			return nil, nil, fmt.Errorf("invalid language in language_limits: %s", language)
		}
		timeLimitMs := item.TimeLimitMs
		if timeLimitMs <= 0 {
			timeLimitMs = defaultTimeMs
		}
		memoryLimitMb := item.MemoryLimitMb
		if memoryLimitMb <= 0 {
			memoryLimitMb = defaultMemoryMb
		}
		if err := validateLimits(timeLimitMs, memoryLimitMb, false); err != nil {
			return nil, nil, fmt.Errorf("%s: %w", language, err)
		}
		overrides = append(overrides, packagefs.LanguageLimit{
			Language: language,
			TimeMs:   timeLimitMs,
			MemoryMb: memoryLimitMb,
		})
	}

	return languageLimitMapToSlices(packagefs.DefaultLanguageLimits(defaultTimeMs, defaultMemoryMb, overrides))
}

func normalizeComponents(
	runner types.ProblemComponentInput,
	checker types.ProblemComponentInput,
	validator types.ProblemComponentInput,
	scorer types.ProblemComponentInput,
) (packagefs.ComponentSet, error) {
	var components packagefs.ComponentSet
	var err error
	if components.Runner, err = normalizeComponentInput("runner", runner); err != nil {
		return components, err
	}
	if components.Checker, err = normalizeComponentInput("checker", checker); err != nil {
		return components, err
	}
	if components.Validator, err = normalizeComponentInput("validator", validator); err != nil {
		return components, err
	}
	if components.Scorer, err = normalizeComponentInput("scorer", scorer); err != nil {
		return components, err
	}
	return components, nil
}

func normalizeComponentInput(kind string, raw types.ProblemComponentInput) (packagefs.ComponentSpec, error) {
	spec := packagefs.ComponentSpec{
		Type:       strings.ToLower(strings.TrimSpace(raw.Type)),
		Name:       strings.TrimSpace(raw.Name),
		Language:   strings.ToLower(strings.TrimSpace(raw.Language)),
		SourcePath: strings.TrimSpace(raw.SourcePath),
		SourceCode: raw.SourceCode,
		Args:       compactArgs(raw.Args),
	}
	if !componentInputProvided(raw) {
		return spec, nil
	}
	if spec.Type == "" {
		spec.Type = "builtin"
	}
	if spec.Type != "builtin" && spec.Type != "custom" {
		return spec, fmt.Errorf("%s.type must be builtin or custom", kind)
	}
	if spec.Name == "" {
		return spec, fmt.Errorf("%s.name is required", kind)
	}
	if spec.Language != "" && !languagePattern.MatchString(spec.Language) {
		return spec, fmt.Errorf("%s.language is invalid: %s", kind, spec.Language)
	}
	if spec.SourcePath != "" {
		if err := validateRelativeLogicalPath(spec.SourcePath); err != nil {
			return spec, fmt.Errorf("%s.source_path: %w", kind, err)
		}
	}
	if spec.Type == "custom" && spec.SourcePath == "" && spec.SourceCode == "" {
		return spec, fmt.Errorf("%s custom component requires source_path or source_code", kind)
	}
	return spec, nil
}

func componentInputProvided(raw types.ProblemComponentInput) bool {
	return strings.TrimSpace(raw.Type) != "" ||
		strings.TrimSpace(raw.Name) != "" ||
		strings.TrimSpace(raw.Language) != "" ||
		strings.TrimSpace(raw.SourcePath) != "" ||
		raw.SourceCode != "" ||
		raw.Args != nil
}

func compactArgs(args []string) []string {
	if args == nil {
		return nil
	}
	out := make([]string, 0, len(args))
	for _, arg := range args {
		arg = strings.TrimSpace(arg)
		if arg != "" {
			out = append(out, arg)
		}
	}
	return out
}

func validateRelativeLogicalPath(logical string) error {
	logical = strings.TrimSpace(strings.ReplaceAll(logical, "\\", "/"))
	if logical == "" {
		return fmt.Errorf("empty relative path")
	}
	if strings.HasPrefix(logical, "/") {
		return fmt.Errorf("absolute path is not allowed")
	}
	for _, part := range strings.Split(logical, "/") {
		if part == ".." {
			return fmt.Errorf("parent path is not allowed")
		}
	}
	return nil
}

func languageLimitMapToSlices(limits map[string]packagefs.LimitConfig) ([]repository.ProblemLanguageLimit, []packagefs.LanguageLimit, error) {
	languages := make([]string, 0, len(limits))
	for language := range limits {
		languages = append(languages, language)
	}
	sort.Strings(languages)

	repoLimits := make([]repository.ProblemLanguageLimit, 0, len(languages))
	packageLimits := make([]packagefs.LanguageLimit, 0, len(languages))
	for _, language := range languages {
		limit := limits[language]
		if err := validateLimits(limit.TimeMs, limit.MemoryMb, false); err != nil {
			return nil, nil, fmt.Errorf("%s: %w", language, err)
		}
		repoLimits = append(repoLimits, repository.ProblemLanguageLimit{
			Language:      language,
			TimeLimitMs:   limit.TimeMs,
			MemoryLimitMb: limit.MemoryMb,
		})
		packageLimits = append(packageLimits, packagefs.LanguageLimit{
			Language: language,
			TimeMs:   limit.TimeMs,
			MemoryMb: limit.MemoryMb,
		})
	}
	return repoLimits, packageLimits, nil
}

func validateLimits(timeLimitMs int, memoryLimitMb int, allowZero bool) error {
	if !allowZero || timeLimitMs != 0 {
		if timeLimitMs <= 0 || timeLimitMs > 600000 {
			return fmt.Errorf("time_limit_ms must be between 1 and 600000")
		}
	}

	if !allowZero || memoryLimitMb != 0 {
		if memoryLimitMb <= 0 || memoryLimitMb > 65536 {
			return fmt.Errorf("memory_limit_mb must be between 1 and 65536")
		}
	}

	return nil
}

func parseTags(raw string) []string {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return []string{}
	}

	return parseTagList(raw)
}

func parseTagsForPut(raw string) []string {
	return parseTagList(raw)
}

func parseTagList(raw string) []string {
	raw = strings.TrimSpace(raw)

	seen := make(map[string]bool)
	tags := make([]string, 0)
	for _, part := range strings.Split(raw, ",") {
		tag := strings.ToLower(strings.TrimSpace(part))
		if tag == "" || seen[tag] {
			continue
		}
		seen[tag] = true
		tags = append(tags, tag)
	}

	return tags
}
