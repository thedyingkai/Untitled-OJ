package packagefs

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"gopkg.in/yaml.v3"
)

const (
	maxPackageFileBytes = int64(64 * 1024 * 1024)
	maxPackageBytes     = int64(512 * 1024 * 1024)
	maxYAMLBytes        = int64(1 * 1024 * 1024)
)

type PackageComponentSummary struct {
	Type       string
	Name       string
	ConfigPath string
}

type PackageLanguageLimit struct {
	Language string
	TimeMs   int
	MemoryMb int
}

type PackageLimitsSummary struct {
	DefaultTimeMs   int
	DefaultMemoryMb int
	Languages       []PackageLanguageLimit
}

type PackageSummary struct {
	Schema         string
	ProblemNo      string
	Slug           string
	Title          string
	Type           string
	Visibility     string
	Status         string
	SourceFormat   string
	ManifestSha256 string
	TotalCases     int
	TotalScore     int
	SampleCount    int
	FileCount      int
	SizeBytes      int64
	Limits         PackageLimitsSummary
	Runner         PackageComponentSummary
	Checker        PackageComponentSummary
	Validator      PackageComponentSummary
	Scorer         PackageComponentSummary
}

type PackageValidationIssue struct {
	Level   string
	Code    string
	Message string
	Path    string
	CaseNo  int
}

type PackageValidationResult struct {
	Valid    bool
	Errors   []PackageValidationIssue
	Warnings []PackageValidationIssue
}

type PackageInspection struct {
	Summary    PackageSummary
	Validation PackageValidationResult
	Cases      []CaseRecord
}

type packageInspector struct {
	packageDir string
	errors     []PackageValidationIssue
	warnings   []PackageValidationIssue
}

func InspectPackage(packageDir string) (*PackageInspection, error) {
	packageDir = strings.TrimSpace(packageDir)
	if packageDir == "" {
		return nil, errors.New("empty package dir")
	}

	inspector := &packageInspector{packageDir: packageDir}
	summary := PackageSummary{}

	fileCount, sizeBytes := inspector.scanFiles()
	summary.FileCount = fileCount
	summary.SizeBytes = sizeBytes

	var manifest ProblemManifest
	manifestLoaded := inspector.readYAML("problem.yaml", &manifest)
	if manifestLoaded {
		summary.Schema = manifest.Schema
		summary.ProblemNo = manifest.ProblemNo
		summary.Slug = manifest.Slug
		summary.Title = manifest.Title
		summary.Type = manifest.Type
		summary.Visibility = manifest.Visibility
		summary.Status = manifest.Status
		summary.SourceFormat = manifest.Source.Format
		summary.ManifestSha256 = inspector.fileSHA("problem.yaml")
		summary.Limits = summarizeLimits(manifest.Limits)

		inspector.validateManifest(manifest)
		summary.Runner = inspector.validateComponent("runner", manifest.Runner, allowedBuiltinRunners())
		summary.Checker = inspector.validateComponent("checker", manifest.Checker, allowedBuiltinCheckers())
		summary.Validator = inspector.validateComponent("validator", manifest.Validator, allowedBuiltinValidators())
		summary.Scorer = inspector.validateComponent("scorer", manifest.Scorer, allowedBuiltinScorers())
	} else if inspector.exists("cases.yaml") || inspector.exists("test_cases.yaml") {
		inspector.addError("legacy_format", "legacy package format is not accepted", "")
	}

	var cases []CaseRecord
	if manifestLoaded {
		cases = inspector.validateCases(manifest)
		summary.TotalCases = len(cases)
		for _, c := range cases {
			summary.TotalScore += c.Score
			if c.Sample {
				summary.SampleCount++
			}
		}
	}

	validation := PackageValidationResult{
		Valid:    len(inspector.errors) == 0,
		Errors:   inspector.errors,
		Warnings: inspector.warnings,
	}

	return &PackageInspection{
		Summary:    summary,
		Validation: validation,
		Cases:      cases,
	}, nil
}

func ValidatePackage(packageDir string) (*PackageValidationResult, error) {
	inspection, err := InspectPackage(packageDir)
	if err != nil {
		return nil, err
	}
	return &inspection.Validation, nil
}

func (i *packageInspector) validateManifest(manifest ProblemManifest) {
	if manifest.Schema != "ojos.problem.v1" {
		i.addError("invalid_schema", "problem.yaml schema must be ojos.problem.v1", "problem.yaml")
	}
	if manifest.ID <= 0 {
		i.addError("invalid_problem_id", "problem.yaml id must be positive", "problem.yaml")
	}
	if strings.TrimSpace(manifest.Slug) == "" {
		i.addError("empty_slug", "problem.yaml slug is required", "problem.yaml")
	}
	if strings.TrimSpace(manifest.Title) == "" {
		i.addError("empty_title", "problem.yaml title is required", "problem.yaml")
	}
	if strings.TrimSpace(manifest.ProblemNo) == "" {
		i.addError("empty_problem_no", "problem.yaml problem_no is required", "problem.yaml")
	}
	if !oneOf(manifest.Type, "traditional", "interactive", "communication", "output_only", "heuristic") {
		i.addError("invalid_type", "problem.yaml type is invalid", "problem.yaml")
	}
	if !oneOf(manifest.Visibility, "private", "public") {
		i.addError("invalid_visibility", "problem.yaml visibility is invalid", "problem.yaml")
	}
	if !oneOf(manifest.Status, "draft", "ready", "published", "archived") {
		i.addError("invalid_status", "problem.yaml status is invalid", "problem.yaml")
	}

	i.validateLimit("limits.default", manifest.Limits.Default.TimeMs, manifest.Limits.Default.MemoryMb, "problem.yaml")
	for language, limit := range manifest.Limits.Languages {
		i.validateLimit("limits.languages."+language, limit.TimeMs, limit.MemoryMb, "problem.yaml")
	}

	if strings.TrimSpace(manifest.Tests.Root) == "" {
		i.addError("empty_tests_root", "tests.root is required", "problem.yaml")
	} else if _, err := safeJoin(i.packageDir, manifest.Tests.Root); err != nil {
		i.addError("invalid_tests_root", err.Error(), "problem.yaml")
	}

	if strings.TrimSpace(manifest.Tests.Cases) == "" {
		i.addError("empty_cases_path", "tests.cases is required", "problem.yaml")
	}
	for locale, path := range manifest.Statement.Files {
		if strings.TrimSpace(locale) == "" {
			i.addError("empty_statement_locale", "statement locale is empty", "problem.yaml")
		}
		i.validateRelativeExistingFile("statement_file", path, "problem.yaml")
	}
	if manifest.Statement.Format != "" && manifest.Statement.Format != ContentFormatMarkdownLatex {
		i.addError("invalid_statement_format", "statement.format must be markdown+latex", "problem.yaml")
	}
	if manifest.Statement.AssetsDir != "" {
		i.validateRelativePath("statement_assets", manifest.Statement.AssetsDir, "problem.yaml")
	}
	if manifest.Tutorial.Format != "" && manifest.Tutorial.Format != ContentFormatMarkdownLatex {
		i.addError("invalid_tutorial_format", "tutorial.format must be markdown+latex", "problem.yaml")
	}
	for locale, path := range manifest.Tutorial.Files {
		if strings.TrimSpace(locale) == "" {
			i.addError("empty_tutorial_locale", "tutorial locale is empty", "problem.yaml")
		}
		i.validateRelativeExistingFile("tutorial_file", path, "problem.yaml")
	}
	if manifest.Tutorial.Std.Path != "" {
		i.validateRelativeExistingFile("tutorial_std", manifest.Tutorial.Std.Path, "problem.yaml")
	}
}

func (i *packageInspector) validateCases(manifest ProblemManifest) []CaseRecord {
	casesPath := strings.TrimSpace(manifest.Tests.Cases)
	if casesPath == "" {
		return nil
	}

	var casesFile CasesFile
	if !i.readYAML(casesPath, &casesFile) {
		return nil
	}

	if len(casesFile.Cases) == 0 {
		i.addWarning("empty_cases", "tests/cases.yaml exists but has no cases", casesPath)
	}

	groups := i.validateGroups(manifest.Tests.Groups)
	testsRoot, err := safeJoin(i.packageDir, manifest.Tests.Root)
	if err != nil {
		i.addError("invalid_tests_root", err.Error(), "problem.yaml")
		return casesFile.Cases
	}

	seen := map[int]bool{}
	for _, c := range casesFile.Cases {
		path := casesPath
		if c.No <= 0 {
			i.addErrorForCase("invalid_case_no", "case_no must be positive", path, c.No)
		}
		if seen[c.No] {
			i.addErrorForCase("duplicate_case_no", "case_no is duplicated", path, c.No)
		}
		seen[c.No] = true

		if strings.TrimSpace(c.Input) == "" {
			i.addErrorForCase("empty_input", "case input is required", path, c.No)
		} else {
			i.validateCaseDataFile("missing_input", testsRoot, c.Input, c.No)
		}

		if strings.TrimSpace(c.Answer) == "" {
			i.addErrorForCase("empty_answer", "case answer is required", path, c.No)
		} else {
			i.validateCaseDataFile("missing_answer", testsRoot, c.Answer, c.No)
		}

		if c.Score < 0 {
			i.addErrorForCase("invalid_score", "case score must be non-negative", path, c.No)
		}
		if c.Group < 0 {
			i.addErrorForCase("invalid_group", "case group must be non-negative", path, c.No)
		}
		if len(groups) > 0 && !groups[c.Group] {
			i.addErrorForCase("unknown_group", "case references a group not declared in tests/groups.yaml", path, c.No)
		}
		if c.TimeLimitMs != 0 || c.MemoryLimitMb != 0 {
			i.validateLimitForCase(c.TimeLimitMs, c.MemoryLimitMb, path, c.No)
		}
	}

	sort.Slice(casesFile.Cases, func(a, b int) bool {
		return casesFile.Cases[a].No < casesFile.Cases[b].No
	})

	return casesFile.Cases
}

func (i *packageInspector) validateGroups(groupsPath string) map[int]bool {
	groupsPath = strings.TrimSpace(groupsPath)
	if groupsPath == "" {
		return map[int]bool{DefaultGroupNo: true}
	}

	var groups GroupsFile
	if !i.readYAML(groupsPath, &groups) {
		return nil
	}
	if len(groups.Groups) == 0 {
		return map[int]bool{DefaultGroupNo: true}
	}

	seen := map[int]bool{}
	for _, group := range groups.Groups {
		if group.No < 0 {
			i.addError("invalid_group_no", "group_no must be non-negative", groupsPath)
		}
		if seen[group.No] {
			i.addError("duplicate_group_no", fmt.Sprintf("group_no is duplicated: %d", group.No), groupsPath)
		}
		seen[group.No] = true
		if group.Score < 0 {
			i.addError("invalid_group_score", fmt.Sprintf("group %d score must be non-negative", group.No), groupsPath)
		}
		if !oneOf(group.Rule, "", "sum", "min", "max", "any", "all_or_nothing") {
			i.addError("invalid_group_rule", fmt.Sprintf("group %d rule is invalid", group.No), groupsPath)
		}
	}
	return seen
}

func (i *packageInspector) validateComponent(kind string, ref ComponentRef, allowedBuiltin map[string]bool) PackageComponentSummary {
	configPath := strings.TrimSpace(ref.Config)
	summary := PackageComponentSummary{ConfigPath: configPath}
	if configPath == "" {
		i.addError("empty_"+kind+"_config", kind+" config path is required", "problem.yaml")
		return summary
	}

	var config ComponentConfig
	if !i.readYAML(configPath, &config) {
		return summary
	}

	summary.Type = config.Type
	summary.Name = config.Name
	if strings.TrimSpace(config.Type) == "" {
		i.addError("empty_"+kind+"_type", kind+" type is required", configPath)
	}
	if strings.TrimSpace(config.Name) == "" {
		i.addError("empty_"+kind+"_name", kind+" name is required", configPath)
	}
	if config.Type != "" && !oneOf(config.Type, "builtin", "custom") {
		i.addError("invalid_"+kind+"_type", kind+" type must be builtin or custom", configPath)
	}
	if config.Type == "builtin" && config.Name != "" && !allowedBuiltin[config.Name] {
		i.addError("unsupported_"+kind, fmt.Sprintf("unsupported builtin %s: %s", kind, config.Name), configPath)
	}
	if config.Type == "custom" {
		source, ok := config.Config["source"].(string)
		if !ok || strings.TrimSpace(source) == "" {
			i.addError("missing_"+kind+"_source", kind+" custom component requires config.source", configPath)
		} else {
			i.validateRelativeExistingFile(kind+"_source", source, configPath)
		}
	}
	return summary
}

func allowedBuiltinRunners() map[string]bool {
	return map[string]bool{
		"traditional-runner":   true,
		"interactive-runner":   true,
		"communication-runner": true,
		"output-only-runner":   true,
		"heuristic-runner":     true,
	}
}

func allowedBuiltinCheckers() map[string]bool {
	return map[string]bool{
		"default-trim-checker":  true,
		"interactive-checker":   true,
		"communication-checker": true,
		"output-only-checker":   true,
		"heuristic-checker":     true,
	}
}

func allowedBuiltinValidators() map[string]bool {
	return map[string]bool{
		"default-input-validator": true,
	}
}

func allowedBuiltinScorers() map[string]bool {
	return map[string]bool{
		"default-sum-scorer": true,
		"heuristic-scorer":   true,
	}
}

func (i *packageInspector) validateLimit(name string, timeMs int, memoryMb int, path string) {
	if timeMs <= 0 || timeMs > 600000 {
		i.addError("invalid_time_limit", name+" time_ms must be 1..600000", path)
	}
	if memoryMb <= 0 || memoryMb > 65536 {
		i.addError("invalid_memory_limit", name+" memory_mb must be 1..65536", path)
	}
}

func (i *packageInspector) validateLimitForCase(timeMs int, memoryMb int, path string, caseNo int) {
	if timeMs != 0 && (timeMs <= 0 || timeMs > 600000) {
		i.addErrorForCase("invalid_case_time_limit", "case time_limit_ms must be 1..600000 when set", path, caseNo)
	}
	if memoryMb != 0 && (memoryMb <= 0 || memoryMb > 65536) {
		i.addErrorForCase("invalid_case_memory_limit", "case memory_limit_mb must be 1..65536 when set", path, caseNo)
	}
}

func (i *packageInspector) validateRelativePath(code string, logical string, source string) {
	if _, err := safeJoin(i.packageDir, logical); err != nil {
		i.addError(code, err.Error(), source)
	}
}

func (i *packageInspector) validateRelativeExistingFile(code string, logical string, source string) {
	full, err := safeJoin(i.packageDir, logical)
	if err != nil {
		i.addError(code, err.Error(), source)
		return
	}
	stat, err := os.Stat(full)
	if err != nil {
		i.addError(code, fmt.Sprintf("file does not exist: %s", logical), source)
		return
	}
	if stat.IsDir() {
		i.addError(code, fmt.Sprintf("path is a directory: %s", logical), source)
		return
	}
	if stat.Size() > maxPackageFileBytes {
		i.addError(code, fmt.Sprintf("file is too large: %s", logical), source)
	}
}

func (i *packageInspector) validateCaseDataFile(code string, testsRoot string, logical string, caseNo int) {
	full, err := safeJoin(testsRoot, logical)
	if err != nil {
		i.addErrorForCase("path_escape", err.Error(), "tests/cases.yaml", caseNo)
		return
	}
	stat, err := os.Stat(full)
	if err != nil {
		i.addErrorForCase(code, fmt.Sprintf("file does not exist: %s", logical), "tests/cases.yaml", caseNo)
		return
	}
	if stat.IsDir() {
		i.addErrorForCase(code, fmt.Sprintf("path is a directory: %s", logical), "tests/cases.yaml", caseNo)
		return
	}
	if stat.Size() > maxPackageFileBytes {
		i.addErrorForCase("case_file_too_large", fmt.Sprintf("case file is too large: %s", logical), "tests/cases.yaml", caseNo)
	}
}

func (i *packageInspector) scanFiles() (int, int64) {
	var fileCount int
	var sizeBytes int64

	err := filepath.WalkDir(i.packageDir, func(path string, d os.DirEntry, err error) error {
		if err != nil {
			i.addError("walk_error", err.Error(), "")
			return nil
		}
		if path == i.packageDir {
			return nil
		}

		rel, relErr := filepath.Rel(i.packageDir, path)
		if relErr != nil {
			i.addError("path_error", relErr.Error(), "")
			return nil
		}
		logical := filepath.ToSlash(rel)

		if d.Type()&os.ModeSymlink != 0 {
			i.addError("symlink_not_allowed", "symbolic links are not allowed in problem packages", logical)
			return nil
		}
		if d.IsDir() {
			return nil
		}

		info, infoErr := d.Info()
		if infoErr != nil {
			i.addError("stat_error", infoErr.Error(), logical)
			return nil
		}
		fileCount++
		sizeBytes += info.Size()
		if info.Size() > maxPackageFileBytes {
			i.addError("file_too_large", "file exceeds package file size limit", logical)
		}
		if sizeBytes > maxPackageBytes {
			i.addError("package_too_large", "package total size exceeds limit", "")
		}
		return nil
	})
	if err != nil {
		i.addError("walk_error", err.Error(), "")
	}

	return fileCount, sizeBytes
}

func (i *packageInspector) readYAML(logical string, out any) bool {
	full, err := safeJoin(i.packageDir, logical)
	if err != nil {
		i.addError("path_escape", err.Error(), logical)
		return false
	}

	stat, err := os.Stat(full)
	if err != nil {
		if logical == "problem.yaml" {
			i.addError("missing_problem_yaml", "problem.yaml is required", "problem.yaml")
		} else {
			i.addError("missing_yaml", fmt.Sprintf("required YAML file does not exist: %s", logical), logical)
		}
		return false
	}
	if stat.IsDir() {
		i.addError("yaml_is_directory", fmt.Sprintf("YAML path is a directory: %s", logical), logical)
		return false
	}
	if stat.Size() > maxYAMLBytes {
		i.addError("yaml_too_large", fmt.Sprintf("YAML file is too large: %s", logical), logical)
		return false
	}

	data, err := os.ReadFile(full)
	if err != nil {
		i.addError("read_yaml_failed", err.Error(), logical)
		return false
	}
	if err := yaml.Unmarshal(data, out); err != nil {
		i.addError("invalid_yaml", err.Error(), logical)
		return false
	}
	return true
}

func (i *packageInspector) exists(logical string) bool {
	full, err := safeJoin(i.packageDir, logical)
	if err != nil {
		return false
	}
	_, err = os.Stat(full)
	return err == nil
}

func (i *packageInspector) fileSHA(logical string) string {
	full, err := safeJoin(i.packageDir, logical)
	if err != nil {
		return ""
	}
	sha, err := FileSha256(full)
	if err != nil {
		return ""
	}
	return sha
}

func (i *packageInspector) addError(code string, message string, path string) {
	i.errors = append(i.errors, PackageValidationIssue{
		Level:   "error",
		Code:    code,
		Message: message,
		Path:    path,
	})
}

func (i *packageInspector) addWarning(code string, message string, path string) {
	i.warnings = append(i.warnings, PackageValidationIssue{
		Level:   "warning",
		Code:    code,
		Message: message,
		Path:    path,
	})
}

func (i *packageInspector) addErrorForCase(code string, message string, path string, caseNo int) {
	i.errors = append(i.errors, PackageValidationIssue{
		Level:   "error",
		Code:    code,
		Message: message,
		Path:    path,
		CaseNo:  caseNo,
	})
}

func (i *packageInspector) addWarningForCase(code string, message string, path string, caseNo int) {
	i.warnings = append(i.warnings, PackageValidationIssue{
		Level:   "warning",
		Code:    code,
		Message: message,
		Path:    path,
		CaseNo:  caseNo,
	})
}

func summarizeLimits(limits Limits) PackageLimitsSummary {
	languages := make([]PackageLanguageLimit, 0, len(limits.Languages))
	for language, limit := range limits.Languages {
		languages = append(languages, PackageLanguageLimit{
			Language: language,
			TimeMs:   limit.TimeMs,
			MemoryMb: limit.MemoryMb,
		})
	}
	sort.Slice(languages, func(i, j int) bool {
		return languages[i].Language < languages[j].Language
	})

	return PackageLimitsSummary{
		DefaultTimeMs:   limits.Default.TimeMs,
		DefaultMemoryMb: limits.Default.MemoryMb,
		Languages:       languages,
	}
}

func oneOf(value string, allowed ...string) bool {
	value = strings.TrimSpace(value)
	for _, item := range allowed {
		if value == item {
			return true
		}
	}
	return false
}
