// Package objectstoredeploy owns optional provider deployment assets, not the
// storage HTTP/domain contract. Runtime storage never calls these operations.
package objectstoredeploy

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
)

type credential struct {
	AccessKey string `json:"accessKey"`
	SecretKey string `json:"secretKey"`
}

type identity struct {
	Name        string       `json:"name"`
	Credentials []credential `json:"credentials"`
	Actions     []string     `json:"actions,omitempty"`
	PolicyNames []string     `json:"policyNames,omitempty"`
}

type policy struct {
	Name    string `json:"name"`
	Content string `json:"content"`
}

func readCredential(prefix string) (credential, error) {
	key, err := secret(prefix + "ACCESS_KEY")
	if err != nil {
		return credential{}, err
	}
	value, err := secret(prefix + "SECRET_KEY")
	if err != nil {
		return credential{}, err
	}
	if len(key) < 16 || len(value) < 32 {
		return credential{}, fmt.Errorf("%s credentials require an access key of at least 16 and a secret of at least 32 characters", prefix)
	}
	return credential{AccessKey: key, SecretKey: value}, nil
}

func secret(name string) (string, error) {
	value := strings.TrimSpace(os.Getenv(name))
	file := strings.TrimSpace(os.Getenv(name + "_FILE"))
	if file != "" {
		if value != "" {
			return "", fmt.Errorf("set only one of %s and %s_FILE", name, name)
		}
		data, err := os.ReadFile(file)
		if err != nil {
			return "", fmt.Errorf("read %s_FILE: %w", name, err)
		}
		value = strings.TrimSpace(string(data))
	}
	if value == "" || strings.Contains(strings.ToUpper(value), "CHANGE_ME") {
		return "", fmt.Errorf("%s is required; supply a unique credential", name)
	}
	return value, nil
}

var bucketName = regexp.MustCompile(`^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$`)

func buckets() ([]string, error) {
	value := strings.TrimSpace(os.Getenv("OJOS_STORAGE_BUCKETS"))
	if value == "" {
		value = "problems,submissions,judge-artifacts,avatars"
	}
	names := strings.Split(value, ",")
	seen := make(map[string]bool)
	for i, name := range names {
		name = strings.TrimSpace(name)
		if !bucketName.MatchString(name) || strings.Contains(name, "..") || seen[name] {
			return nil, fmt.Errorf("invalid or repeated bucket name %q", name)
		}
		names[i] = name
		seen[name] = true
	}
	return names, nil
}

// RenderSeaweedConfig fails before the provider starts if credentials are absent.
// A missing/empty SeaweedFS identity configuration must never enable anonymous S3.
func RenderSeaweedConfig(output string) error {
	admin, err := readCredential("S3_ADMIN_")
	if err != nil {
		return err
	}
	service, err := readCredential("S3_")
	if err != nil {
		return err
	}
	if admin.AccessKey == service.AccessKey || admin.SecretKey == service.SecretKey {
		return fmt.Errorf("provisioning and application credentials must be different")
	}
	names, err := buckets()
	if err != nil {
		return err
	}
	objects := make([]string, 0, len(names))
	bucketResources := make([]string, 0, len(names))
	for _, name := range names {
		bucketResources = append(bucketResources, "arn:aws:s3:::"+name)
		objects = append(objects, "arn:aws:s3:::"+name+"/*")
	}
	// Native SeaweedFS Write also authorizes bucket lifecycle changes. Use an
	// explicit S3 action allowlist, with no native Read/Write fallback on this user.
	document, err := json.Marshal(map[string]any{
		"Version": "2012-10-17",
		"Statement": []map[string]any{
			{"Effect": "Allow", "Action": []string{"s3:GetObject", "s3:PutObject", "s3:DeleteObject", "s3:AbortMultipartUpload", "s3:ListMultipartUploadParts"}, "Resource": objects},
			{"Effect": "Allow", "Action": []string{"s3:ListBucket", "s3:GetBucketLocation", "s3:ListBucketMultipartUploads"}, "Resource": bucketResources},
		},
	})
	if err != nil {
		return err
	}
	config := struct {
		Identities []identity `json:"identities"`
		Policies   []policy   `json:"policies"`
	}{Identities: []identity{
		{Name: "ojos-provisioner", Credentials: []credential{admin}, Actions: []string{"Admin"}},
		{Name: "ojos-storage", Credentials: []credential{service}, PolicyNames: []string{"ojos-storage-objects"}},
	}, Policies: []policy{{Name: "ojos-storage-objects", Content: string(document)}}}
	data, err := json.Marshal(config)
	if err != nil {
		return err
	}
	file, err := os.CreateTemp(filepath.Dir(output), ".ojos-s3-*")
	if err != nil {
		return err
	}
	defer os.Remove(file.Name())
	if _, err := file.Write(data); err != nil {
		file.Close()
		return err
	}
	if err := file.Close(); err != nil {
		return err
	}
	return os.Rename(file.Name(), output)
}
