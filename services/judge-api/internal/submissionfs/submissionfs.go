package submissionfs

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

type CreateSubmissionFilesArgs struct {
	Root         string
	SubmissionID int64
	Language     string
	Code         string
}

type CreateSubmissionFilesResult struct {
	SubmissionDir string
	CodePath      string
	CodeSha256    string
	ResultPath    string
}

func CreateSubmissionFiles(arg CreateSubmissionFilesArgs) (*CreateSubmissionFilesResult, error) {
	if arg.Root == "" {
		return nil, errors.New("empty submissions root")
	}
	if arg.SubmissionID <= 0 {
		return nil, errors.New("invalid submission id")
	}
	if strings.TrimSpace(arg.Language) == "" {
		arg.Language = "cpp17"
	}
	if arg.Code == "" {
		return nil, errors.New("empty code")
	}

	submissionDir := filepath.Join(arg.Root, fmt.Sprintf("%d", arg.SubmissionID))
	sourceDir := filepath.Join(submissionDir, "source")
	buildDir := filepath.Join(submissionDir, "build")
	casesDir := filepath.Join(submissionDir, "cases")
	resultPath := filepath.Join(submissionDir, "result.json")

	for _, path := range []string{sourceDir, buildDir, casesDir, resultPath} {
		if err := os.RemoveAll(path); err != nil {
			return nil, err
		}
	}

	for _, dir := range []string{sourceDir, buildDir, casesDir} {
		if err := os.MkdirAll(dir, 0755); err != nil {
			return nil, err
		}
	}

	filename, err := SourceFilename(arg.Language)
	if err != nil {
		return nil, err
	}

	codePath := filepath.Join(sourceDir, filename)
	if err := os.WriteFile(codePath, []byte(arg.Code), 0644); err != nil {
		return nil, err
	}

	sha, err := FileSha256(codePath)
	if err != nil {
		return nil, err
	}

	if err := os.WriteFile(resultPath, []byte(`{"cases":[]}`+"\n"), 0644); err != nil {
		return nil, err
	}

	return &CreateSubmissionFilesResult{
		SubmissionDir: submissionDir,
		CodePath:      filepath.ToSlash(codePath),
		CodeSha256:    sha,
		ResultPath:    filepath.ToSlash(resultPath),
	}, nil
}

func SourceFilename(language string) (string, error) {
	switch strings.TrimSpace(language) {
	case "", "cpp", "cpp17", "cpp20":
		return "main.cpp", nil
	case "c", "c11", "c17":
		return "main.c", nil
	case "java", "java17":
		return "Main.java", nil
	case "python", "python3", "py3":
		return "main.py", nil
	default:
		return "", fmt.Errorf("unsupported language: %s", language)
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
