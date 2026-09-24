package objectstoredeploy

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/minio/minio-go/v7"
	"github.com/minio/minio-go/v7/pkg/credentials"
	"github.com/minio/minio-go/v7/pkg/lifecycle"
)

func adminClient() (*minio.Client, error) {
	admin, err := readCredential("S3_ADMIN_")
	if err != nil {
		return nil, err
	}
	endpoint := strings.TrimSpace(os.Getenv("S3_ENDPOINT"))
	if endpoint == "" {
		return nil, fmt.Errorf("S3_ENDPOINT is required")
	}
	secure := strings.TrimSpace(os.Getenv("S3_USE_SSL"))
	if secure != "" && secure != "true" && secure != "false" {
		return nil, fmt.Errorf("S3_USE_SSL must be true or false")
	}
	return minio.New(endpoint, &minio.Options{
		Creds:  credentials.NewStaticV4(admin.AccessKey, admin.SecretKey, ""),
		Secure: secure == "true", Region: strings.TrimSpace(os.Getenv("S3_REGION")),
		BucketLookup: minio.BucketLookupPath, MaxRetries: 1,
	})
}

func Ready(ctx context.Context) error {
	client, err := adminClient()
	if err != nil {
		return err
	}
	_, err = client.ListBuckets(ctx)
	return err
}

// Provision only creates configured buckets and adds an owned temporary-upload
// rule. It never expires user objects or replaces operator lifecycle rules.
func Provision(ctx context.Context) error {
	client, err := adminClient()
	if err != nil {
		return err
	}
	names, err := buckets()
	if err != nil {
		return err
	}
	for _, name := range names {
		exists, err := client.BucketExists(ctx, name)
		if err != nil {
			return fmt.Errorf("check bucket %s: %w", name, err)
		}
		if !exists {
			if err := client.MakeBucket(ctx, name, minio.MakeBucketOptions{Region: os.Getenv("S3_REGION")}); err != nil {
				return fmt.Errorf("create bucket %s: %w", name, err)
			}
		}
		config, err := client.GetBucketLifecycle(ctx, name)
		if err != nil {
			if minio.ToErrorResponse(err).Code != "NoSuchLifecycleConfiguration" {
				return err
			}
			config = &lifecycle.Configuration{}
		}
		found := false
		for _, rule := range config.Rules {
			if rule.ID == "ojos-incomplete-upload" {
				found = true
			}
		}
		if !found {
			config.Rules = append(config.Rules, lifecycle.Rule{
				ID: "ojos-incomplete-upload", Status: "Enabled",
				RuleFilter:                     lifecycle.Filter{Prefix: ".ojos-upload/"},
				Expiration:                     lifecycle.Expiration{Days: 1},
				AbortIncompleteMultipartUpload: lifecycle.AbortIncompleteMultipartUpload{DaysAfterInitiation: 1},
			})
			if err := client.SetBucketLifecycle(ctx, name, config); err != nil {
				return err
			}
		}
	}
	return nil
}
