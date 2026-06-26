package packagefs

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"mime"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"

	"gopkg.in/yaml.v3"
)

type IndexedFile struct {
	LogicalPath string
	FileKind    string
	StoragePath string
	Sha256      string
	SizeBytes   int64
	MimeType    string
}

type LimitConfig struct {
	TimeMs   int `yaml:"time_ms"`
	MemoryMb int `yaml:"memory_mb"`
}

type Limits struct {
	Default   LimitConfig            `yaml:"default"`
	Languages map[string]LimitConfig `yaml:"languages"`
}

type StatementRef struct {
	DefaultLocale string            `yaml:"default_locale"`
	Files         map[string]string `yaml:"files"`
	AssetsDir     string            `yaml:"assets_dir"`
}

type ComponentRef struct {
	Config string `yaml:"config"`
}

type TestsRef struct {
	Root   string `yaml:"root"`
	Groups string `yaml:"groups"`
	Cases  string `yaml:"cases"`
}

type TutorialRef struct {
	DefaultLocale string            `yaml:"default_locale"`
	Files         map[string]string `yaml:"files"`
	Std           TutorialStd       `yaml:"std"`
}

type TutorialStd struct {
	Language string `yaml:"language"`
	Path     string `yaml:"path"`
}

type SourceRef struct {
	Format      string `yaml:"format"`
	Fingerprint string `yaml:"fingerprint"`
}

type ProblemManifest struct {
	Schema     string       `yaml:"schema"`
	ID         int64        `yaml:"id"`
	Slug       string       `yaml:"slug"`
	Title      string       `yaml:"title"`
	Type       string       `yaml:"type"`
	Visibility string       `yaml:"visibility"`
	Status     string       `yaml:"status"`
	Limits     Limits       `yaml:"limits"`
	Statement  StatementRef `yaml:"statement"`
	Runner     ComponentRef `yaml:"runner"`
	Checker    ComponentRef `yaml:"checker"`
	Scorer     ComponentRef `yaml:"scorer"`
	Tests      TestsRef     `yaml:"tests"`
	Tutorial   TutorialRef  `yaml:"tutorial"`
	Source     SourceRef    `yaml:"source"`
}

type ComponentConfig struct {
	Type   string         `yaml:"type"`
	Name   string         `yaml:"name"`
	Config map[string]any `yaml:"config"`
}

type GroupsFile struct {
	Groups []GroupRecord `yaml:"groups"`
}

type GroupRecord struct {
	No       int    `yaml:"group_no"`
	Name     string `yaml:"name"`
	Score    int    `yaml:"score"`
	Rule     string `yaml:"rule"`
	Feedback string `yaml:"feedback"`
}

type CasesFile struct {
	Cases []CaseRecord `yaml:"cases"`
}

type CaseRecord struct {
	No            int    `yaml:"case_no"`
	Input         string `yaml:"input"`
	Answer        string `yaml:"answer"`
	Score         int    `yaml:"score"`
	Group         int    `yaml:"group"`
	Sample        bool   `yaml:"sample"`
	Hidden        bool   `yaml:"hidden"`
	TimeLimitMs   int    `yaml:"time_limit_ms,omitempty"`
	MemoryLimitMb int    `yaml:"memory_limit_mb,omitempty"`
}

type SampleRecord struct {
	CaseNo int
	Input  string
	Output string
}

type CreateProblemArgs struct {
	Root          string
	ID            int64
	Slug          string
	Title         string
	Statement     string
	ProblemType   string
	Visibility    string
	TimeLimitMs   int
	MemoryLimitMb int
}

type CreateProblemResult struct {
	PackageDir     string
	ManifestPath   string
	ManifestSha256 string
	Files          []IndexedFile
}

func Slugify(s string) string {
	s = strings.ToLower(strings.TrimSpace(s))
	if s == "" {
		return "problem"
	}

	re := regexp.MustCompile(`[^a-z0-9]+`)
	s = re.ReplaceAllString(s, "-")
	s = strings.Trim(s, "-")
	if s == "" {
		return "problem"
	}

	return s
}

func CreateInitialPackage(arg CreateProblemArgs) (*CreateProblemResult, error) {
	if arg.Root == "" {
		return nil, errors.New("empty problems root")
	}

	if arg.ProblemType == "" {
		arg.ProblemType = "traditional"
	}
	if arg.Visibility == "" {
		arg.Visibility = "private"
	}
	if arg.TimeLimitMs <= 0 {
		arg.TimeLimitMs = 1000
	}
	if arg.MemoryLimitMb <= 0 {
		arg.MemoryLimitMb = 256
	}

	baseSlug := Slugify(arg.Slug)
	if arg.Slug == "" {
		baseSlug = Slugify(arg.Title)
	}
	finalSlug := fmt.Sprintf("%d-%s", arg.ID, baseSlug)

	packageDir := filepath.Join(arg.Root, finalSlug)

	dirs := []string{
		"statement/assets",
		"attachments",
		"tests",
		"checker",
		"runner",
		"scorer",
		"validators",
		"generators",
		"tutorial",
	}

	for _, dir := range dirs {
		if err := os.MkdirAll(filepath.Join(packageDir, dir), 0755); err != nil {
			return nil, err
		}
	}

	statement := strings.TrimSpace(arg.Statement)
	if statement == "" {
		statement = fmt.Sprintf("# %s\n\nDescribe the task, input format, output format, constraints and samples here.\n", arg.Title)
	}

	files := map[string][]byte{}

	files["statement/zh-cn.md"] = []byte(statement + "\n")
	files["tutorial/zh-cn.md"] = []byte("# Tutorial\n\nAdd the official solution explanation here.\n")
	files["tutorial/std.cpp"] = []byte(`#include <bits/stdc++.h>
using namespace std;

int main() {
    return 0;
}
`)

	files["runner/runner.yaml"] = mustYAML(ComponentConfig{
		Type:   "builtin",
		Name:   "traditional-runner",
		Config: map[string]any{},
	})

	files["checker/checker.yaml"] = mustYAML(ComponentConfig{
		Type: "builtin",
		Name: "default-trim-checker",
		Config: map[string]any{
			"trim_trailing_spaces":        true,
			"ignore_trailing_blank_lines": true,
		},
	})

	files["scorer/scorer.yaml"] = mustYAML(ComponentConfig{
		Type:   "builtin",
		Name:   "default-sum-scorer",
		Config: map[string]any{},
	})

	files["tests/groups.yaml"] = mustYAML(GroupsFile{
		Groups: []GroupRecord{
			{
				No:       0,
				Name:     "default",
				Score:    100,
				Rule:     "sum",
				Feedback: "full",
			},
		},
	})

	files["tests/cases.yaml"] = mustYAML(CasesFile{
		Cases: []CaseRecord{},
	})

	manifest := ProblemManifest{
		Schema:     "ojos.problem.v1",
		ID:         arg.ID,
		Slug:       finalSlug,
		Title:      arg.Title,
		Type:       arg.ProblemType,
		Visibility: arg.Visibility,
		Status:     "draft",
		Limits: Limits{
			Default: LimitConfig{
				TimeMs:   arg.TimeLimitMs,
				MemoryMb: arg.MemoryLimitMb,
			},
			Languages: map[string]LimitConfig{
				"cpp17": {
					TimeMs:   arg.TimeLimitMs,
					MemoryMb: arg.MemoryLimitMb,
				},
				"cpp20": {
					TimeMs:   arg.TimeLimitMs,
					MemoryMb: arg.MemoryLimitMb,
				},
				"c11": {
					TimeMs:   arg.TimeLimitMs,
					MemoryMb: arg.MemoryLimitMb,
				},
				"python3": {
					TimeMs:   arg.TimeLimitMs * 3,
					MemoryMb: arg.MemoryLimitMb * 2,
				},
				"java17": {
					TimeMs:   arg.TimeLimitMs * 2,
					MemoryMb: arg.MemoryLimitMb * 2,
				},
			},
		},
		Statement: StatementRef{
			DefaultLocale: "zh-cn",
			Files: map[string]string{
				"zh-cn": "statement/zh-cn.md",
			},
			AssetsDir: "statement/assets",
		},
		Runner: ComponentRef{
			Config: "runner/runner.yaml",
		},
		Checker: ComponentRef{
			Config: "checker/checker.yaml",
		},
		Scorer: ComponentRef{
			Config: "scorer/scorer.yaml",
		},
		Tests: TestsRef{
			Root:   "tests",
			Groups: "tests/groups.yaml",
			Cases:  "tests/cases.yaml",
		},
		Tutorial: TutorialRef{
			DefaultLocale: "zh-cn",
			Files: map[string]string{
				"zh-cn": "tutorial/zh-cn.md",
			},
			Std: TutorialStd{
				Language: "cpp17",
				Path:     "tutorial/std.cpp",
			},
		},
		Source: SourceRef{
			Format:      "ojos",
			Fingerprint: "",
		},
	}

	files["problem.yaml"] = mustYAML(manifest)

	for logicalPath, content := range files {
		full := filepath.Join(packageDir, filepath.FromSlash(logicalPath))
		if err := os.WriteFile(full, content, 0644); err != nil {
			return nil, err
		}
	}

	indexed, err := IndexFiles(packageDir)
	if err != nil {
		return nil, err
	}

	manifestSha, err := FileSha256(filepath.Join(packageDir, "problem.yaml"))
	if err != nil {
		return nil, err
	}

	return &CreateProblemResult{
		PackageDir:     packageDir,
		ManifestPath:   "problem.yaml",
		ManifestSha256: manifestSha,
		Files:          indexed,
	}, nil
}

func syncBuiltinLanguageLimits(manifest *ProblemManifest) {
	if manifest.Limits.Languages == nil {
		manifest.Limits.Languages = map[string]LimitConfig{}
	}

	t := manifest.Limits.Default.TimeMs
	m := manifest.Limits.Default.MemoryMb

	if t <= 0 {
		t = 1000
	}
	if m <= 0 {
		m = 256
	}

	manifest.Limits.Languages["cpp17"] = LimitConfig{
		TimeMs:   t,
		MemoryMb: m,
	}
	manifest.Limits.Languages["cpp20"] = LimitConfig{
		TimeMs:   t,
		MemoryMb: m,
	}
	manifest.Limits.Languages["c11"] = LimitConfig{
		TimeMs:   t,
		MemoryMb: m,
	}
	manifest.Limits.Languages["python3"] = LimitConfig{
		TimeMs:   t * 3,
		MemoryMb: m * 2,
	}
	manifest.Limits.Languages["java17"] = LimitConfig{
		TimeMs:   t * 2,
		MemoryMb: m * 2,
	}
}

func UpdateManifest(
	packageDir string,
	title string,
	statement string,
	problemType string,
	visibility string,
	status string,
	timeLimitMs int,
	memoryLimitMb int,
) (string, []IndexedFile, error) {
	manifestPath := filepath.Join(packageDir, "problem.yaml")

	var manifest ProblemManifest
	if err := readYAML(manifestPath, &manifest); err != nil {
		return "", nil, err
	}

	if title != "" {
		manifest.Title = title
	}
	if problemType != "" {
		manifest.Type = problemType
	}
	if visibility != "" {
		manifest.Visibility = visibility
	}
	if status != "" {
		manifest.Status = status
	}
	if timeLimitMs > 0 {
		manifest.Limits.Default.TimeMs = timeLimitMs
	}
	if memoryLimitMb > 0 {
		manifest.Limits.Default.MemoryMb = memoryLimitMb
	}
	if timeLimitMs > 0 || memoryLimitMb > 0 {
		syncBuiltinLanguageLimits(&manifest)
	}

	if statement != "" {
		statementPath := filepath.Join(packageDir, "statement", "zh-cn.md")
		if err := os.WriteFile(statementPath, []byte(statement+"\n"), 0644); err != nil {
			return "", nil, err
		}
	}

	if err := writeYAML(manifestPath, manifest); err != nil {
		return "", nil, err
	}

	indexed, err := IndexFiles(packageDir)
	if err != nil {
		return "", nil, err
	}

	sha, err := FileSha256(manifestPath)
	if err != nil {
		return "", nil, err
	}

	return sha, indexed, nil
}

type AddCaseArgs struct {
	PackageDir    string
	CaseNo        int
	Input         string
	Answer        string
	Score         int
	Group         int
	Sample        bool
	Hidden        bool
	TimeLimitMs   int
	MemoryLimitMb int
}

type AddCaseResult struct {
	CaseNo     int
	InputPath  string
	AnswerPath string
	Files      []IndexedFile
}

func AddCase(arg AddCaseArgs) (*AddCaseResult, error) {
	if arg.PackageDir == "" {
		return nil, errors.New("empty package dir")
	}
	if arg.Input == "" {
		return nil, errors.New("empty input")
	}
	if arg.Answer == "" {
		return nil, errors.New("empty answer")
	}
	if arg.Score <= 0 {
		arg.Score = 100
	}
	if !arg.Sample && !arg.Hidden {
		arg.Hidden = true
	}

	casesPath := filepath.Join(arg.PackageDir, "tests", "cases.yaml")

	var cases CasesFile
	if err := readYAML(casesPath, &cases); err != nil {
		return nil, err
	}

	caseNo := arg.CaseNo
	if caseNo <= 0 {
		caseNo = 1
		for _, c := range cases.Cases {
			if c.No >= caseNo {
				caseNo = c.No + 1
			}
		}
	}

	for _, c := range cases.Cases {
		if c.No == caseNo {
			return nil, fmt.Errorf("case no already exists: %d", caseNo)
		}
	}

	inputPath := fmt.Sprintf("%03d.in", caseNo)
	answerPath := fmt.Sprintf("%03d.ans", caseNo)

	if err := os.WriteFile(filepath.Join(arg.PackageDir, "tests", inputPath), []byte(arg.Input), 0644); err != nil {
		return nil, err
	}

	if err := os.WriteFile(filepath.Join(arg.PackageDir, "tests", answerPath), []byte(arg.Answer), 0644); err != nil {
		return nil, err
	}

	cases.Cases = append(cases.Cases, CaseRecord{
		No:            caseNo,
		Input:         inputPath,
		Answer:        answerPath,
		Score:         arg.Score,
		Group:         arg.Group,
		Sample:        arg.Sample,
		Hidden:        arg.Hidden,
		TimeLimitMs:   arg.TimeLimitMs,
		MemoryLimitMb: arg.MemoryLimitMb,
	})

	sort.Slice(cases.Cases, func(i, j int) bool {
		return cases.Cases[i].No < cases.Cases[j].No
	})

	if err := writeYAML(casesPath, cases); err != nil {
		return nil, err
	}

	changed := []string{
		"tests/" + inputPath,
		"tests/" + answerPath,
		"tests/cases.yaml",
	}

	files, err := IndexSelectedFiles(arg.PackageDir, changed)
	if err != nil {
		return nil, err
	}

	return &AddCaseResult{
		CaseNo:     caseNo,
		InputPath:  "tests/" + inputPath,
		AnswerPath: "tests/" + answerPath,
		Files:      files,
	}, nil
}

func ListCases(packageDir string) ([]CaseRecord, error) {
	casesPath := filepath.Join(packageDir, "tests", "cases.yaml")

	var cases CasesFile
	if err := readYAML(casesPath, &cases); err != nil {
		return nil, err
	}

	sort.Slice(cases.Cases, func(i, j int) bool {
		return cases.Cases[i].No < cases.Cases[j].No
	})

	return cases.Cases, nil
}

func ReadSamples(packageDir string) ([]SampleRecord, error) {
	const maxSampleBytes = 64 * 1024

	cases, err := ListCases(packageDir)
	if err != nil {
		return nil, err
	}

	testsRoot, err := safeJoin(packageDir, "tests")
	if err != nil {
		return nil, err
	}

	samples := make([]SampleRecord, 0)
	for _, c := range cases {
		if !c.Sample {
			continue
		}

		inputPath, err := safeJoin(testsRoot, c.Input)
		if err != nil {
			return nil, err
		}
		answerPath, err := safeJoin(testsRoot, c.Answer)
		if err != nil {
			return nil, err
		}

		input, err := readSmallTextFile(inputPath, maxSampleBytes)
		if err != nil {
			return nil, err
		}
		answer, err := readSmallTextFile(answerPath, maxSampleBytes)
		if err != nil {
			return nil, err
		}

		samples = append(samples, SampleRecord{
			CaseNo: c.No,
			Input:  input,
			Output: answer,
		})
	}

	return samples, nil
}

func DeleteCase(packageDir string, caseNo int) ([]string, []IndexedFile, error) {
	if caseNo <= 0 {
		return nil, nil, errors.New("invalid case no")
	}

	casesPath := filepath.Join(packageDir, "tests", "cases.yaml")

	var cases CasesFile
	if err := readYAML(casesPath, &cases); err != nil {
		return nil, nil, err
	}

	nextCases := make([]CaseRecord, 0, len(cases.Cases))
	var deleted *CaseRecord

	for _, c := range cases.Cases {
		if c.No == caseNo {
			cp := c
			deleted = &cp
			continue
		}
		nextCases = append(nextCases, c)
	}

	if deleted == nil {
		return nil, nil, fmt.Errorf("case not found: %d", caseNo)
	}

	cases.Cases = nextCases

	if err := writeYAML(casesPath, cases); err != nil {
		return nil, nil, err
	}

	deletedLogical := []string{
		"tests/" + deleted.Input,
		"tests/" + deleted.Answer,
	}

	for _, logical := range deletedLogical {
		_ = os.Remove(filepath.Join(packageDir, filepath.FromSlash(logical)))
	}

	changedFiles, err := IndexSelectedFiles(packageDir, []string{"tests/cases.yaml"})
	if err != nil {
		return nil, nil, err
	}

	return deletedLogical, changedFiles, nil
}

func IndexFiles(packageDir string) ([]IndexedFile, error) {
	var files []IndexedFile

	err := filepath.WalkDir(packageDir, func(path string, d os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			return nil
		}

		logical, err := filepath.Rel(packageDir, path)
		if err != nil {
			return err
		}

		logical = filepath.ToSlash(logical)

		file, err := indexOne(packageDir, logical)
		if err != nil {
			return err
		}

		files = append(files, file)
		return nil
	})

	if err != nil {
		return nil, err
	}

	sort.Slice(files, func(i, j int) bool {
		return files[i].LogicalPath < files[j].LogicalPath
	})

	return files, nil
}

func IndexSelectedFiles(packageDir string, logicalPaths []string) ([]IndexedFile, error) {
	files := make([]IndexedFile, 0, len(logicalPaths))

	for _, logical := range logicalPaths {
		file, err := indexOne(packageDir, logical)
		if err != nil {
			return nil, err
		}
		files = append(files, file)
	}

	return files, nil
}

func indexOne(packageDir string, logical string) (IndexedFile, error) {
	full := filepath.Join(packageDir, filepath.FromSlash(logical))

	stat, err := os.Stat(full)
	if err != nil {
		return IndexedFile{}, err
	}

	sha, err := FileSha256(full)
	if err != nil {
		return IndexedFile{}, err
	}

	return IndexedFile{
		LogicalPath: logical,
		FileKind:    GuessFileKind(logical),
		StoragePath: full,
		Sha256:      sha,
		SizeBytes:   stat.Size(),
		MimeType:    mime.TypeByExtension(filepath.Ext(logical)),
	}, nil
}

func GuessFileKind(logical string) string {
	switch {
	case logical == "problem.yaml":
		return "manifest"
	case strings.HasPrefix(logical, "statement/assets/"):
		return "asset"
	case strings.HasPrefix(logical, "statement/"):
		return "statement"
	case strings.HasPrefix(logical, "attachments/"):
		return "attachment"
	case strings.HasPrefix(logical, "tests/") && strings.HasSuffix(logical, ".in"):
		return "test_input"
	case strings.HasPrefix(logical, "tests/") && strings.HasSuffix(logical, ".ans"):
		return "test_answer"
	case logical == "tests/cases.yaml":
		return "cases_manifest"
	case logical == "tests/groups.yaml":
		return "groups_manifest"
	case strings.HasPrefix(logical, "checker/"):
		return "checker"
	case strings.HasPrefix(logical, "runner/"):
		return "runner"
	case strings.HasPrefix(logical, "scorer/"):
		return "scorer"
	case strings.HasPrefix(logical, "tutorial/"):
		return "tutorial"
	case strings.HasPrefix(logical, "validators/"):
		return "validator"
	case strings.HasPrefix(logical, "generators/"):
		return "generator"
	default:
		return "other"
	}
}

func FileSha256(path string) (string, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}

	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:]), nil
}

func mustYAML(v any) []byte {
	data, err := yaml.Marshal(v)
	if err != nil {
		panic(err)
	}
	return data
}

func readYAML(path string, out any) error {
	data, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	return yaml.Unmarshal(data, out)
}

func writeYAML(path string, v any) error {
	data, err := yaml.Marshal(v)
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0644)
}

func DeletePackageDir(root string, packageDir string) error {
	if root == "" {
		return errors.New("empty problems root")
	}
	if packageDir == "" {
		return errors.New("empty package dir")
	}

	absRoot, err := filepath.Abs(root)
	if err != nil {
		return err
	}

	absDir, err := filepath.Abs(packageDir)
	if err != nil {
		return err
	}

	rel, err := filepath.Rel(absRoot, absDir)
	if err != nil {
		return err
	}

	if rel == "." || rel == "" {
		return fmt.Errorf("refuse to delete problems root: %s", absDir)
	}

	if strings.HasPrefix(rel, "..") || filepath.IsAbs(rel) {
		return fmt.Errorf("package dir is outside problems root: %s", absDir)
	}

	return os.RemoveAll(absDir)
}

func safeJoin(base string, child string) (string, error) {
	if strings.TrimSpace(child) == "" {
		return "", errors.New("empty relative path")
	}

	cleanChild := filepath.Clean(filepath.FromSlash(child))
	if filepath.IsAbs(cleanChild) {
		return "", fmt.Errorf("absolute path is not allowed: %s", child)
	}

	for _, part := range strings.Split(cleanChild, string(filepath.Separator)) {
		if part == ".." {
			return "", fmt.Errorf("parent path is not allowed: %s", child)
		}
	}

	full := filepath.Join(base, cleanChild)
	absBase, err := filepath.Abs(base)
	if err != nil {
		return "", err
	}
	absFull, err := filepath.Abs(full)
	if err != nil {
		return "", err
	}
	rel, err := filepath.Rel(absBase, absFull)
	if err != nil {
		return "", err
	}
	if strings.HasPrefix(rel, "..") || filepath.IsAbs(rel) {
		return "", fmt.Errorf("path escapes base: %s", child)
	}

	return full, nil
}

func readSmallTextFile(path string, maxBytes int64) (string, error) {
	stat, err := os.Stat(path)
	if err != nil {
		return "", err
	}
	if stat.Size() > maxBytes {
		return "", fmt.Errorf("sample file too large: %s", filepath.Base(path))
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}

	return string(data), nil
}

type UpdateCaseArgs struct {
	PackageDir    string
	CaseNo        int
	Input         string
	Answer        string
	Score         int
	Group         int
	Sample        bool
	Hidden        bool
	TimeLimitMs   int
	MemoryLimitMb int
}

type UpdateCaseResult struct {
	CaseNo     int
	InputPath  string
	AnswerPath string
	Files      []IndexedFile
}

func UpdateCase(arg UpdateCaseArgs) (*UpdateCaseResult, error) {
	if arg.PackageDir == "" {
		return nil, errors.New("empty package dir")
	}
	if arg.CaseNo <= 0 {
		return nil, errors.New("invalid case no")
	}
	if arg.Input == "" {
		return nil, errors.New("empty input")
	}
	if arg.Answer == "" {
		return nil, errors.New("empty answer")
	}

	casesPath := filepath.Join(arg.PackageDir, "tests", "cases.yaml")

	var cases CasesFile
	if err := readYAML(casesPath, &cases); err != nil {
		return nil, err
	}

	found := false
	inputPath := fmt.Sprintf("%03d.in", arg.CaseNo)
	answerPath := fmt.Sprintf("%03d.ans", arg.CaseNo)

	for i := range cases.Cases {
		if cases.Cases[i].No != arg.CaseNo {
			continue
		}

		found = true

		if cases.Cases[i].Input != "" {
			inputPath = cases.Cases[i].Input
		}
		if cases.Cases[i].Answer != "" {
			answerPath = cases.Cases[i].Answer
		}

		score := arg.Score
		if score <= 0 {
			score = cases.Cases[i].Score
		}
		if score <= 0 {
			score = 100
		}

		hidden := arg.Hidden
		if !arg.Sample && !arg.Hidden {
			hidden = true
		}

		cases.Cases[i].Input = inputPath
		cases.Cases[i].Answer = answerPath
		cases.Cases[i].Score = score
		cases.Cases[i].Group = arg.Group
		cases.Cases[i].Sample = arg.Sample
		cases.Cases[i].Hidden = hidden

		if arg.TimeLimitMs > 0 {
			cases.Cases[i].TimeLimitMs = arg.TimeLimitMs
		}
		if arg.MemoryLimitMb > 0 {
			cases.Cases[i].MemoryLimitMb = arg.MemoryLimitMb
		}

		break
	}

	if !found {
		return nil, fmt.Errorf("case not found: %d", arg.CaseNo)
	}

	if err := os.WriteFile(filepath.Join(arg.PackageDir, "tests", inputPath), []byte(arg.Input), 0644); err != nil {
		return nil, err
	}

	if err := os.WriteFile(filepath.Join(arg.PackageDir, "tests", answerPath), []byte(arg.Answer), 0644); err != nil {
		return nil, err
	}

	sort.Slice(cases.Cases, func(i, j int) bool {
		return cases.Cases[i].No < cases.Cases[j].No
	})

	if err := writeYAML(casesPath, cases); err != nil {
		return nil, err
	}

	changed := []string{
		"tests/" + inputPath,
		"tests/" + answerPath,
		"tests/cases.yaml",
	}

	files, err := IndexSelectedFiles(arg.PackageDir, changed)
	if err != nil {
		return nil, err
	}

	return &UpdateCaseResult{
		CaseNo:     arg.CaseNo,
		InputPath:  "tests/" + inputPath,
		AnswerPath: "tests/" + answerPath,
		Files:      files,
	}, nil
}
