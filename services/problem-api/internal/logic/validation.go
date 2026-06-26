package logic

import (
	"fmt"
	"regexp"
	"strings"
)

var slugPattern = regexp.MustCompile(`^[a-z0-9]+(?:-[a-z0-9]+)*$`)

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
	case "private", "public", "contest_only":
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
		return nil
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
