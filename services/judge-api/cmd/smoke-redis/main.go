package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/redis/go-redis/v9"
)

type summary struct {
	Mode          string `json:"mode"`
	Stream        string `json:"stream"`
	ResultStream  string `json:"result_stream,omitempty"`
	Group         string `json:"group"`
	SubmissionID  int64  `json:"submission_id"`
	TaskID        string `json:"task_id,omitempty"`
	TaskEntryID   string `json:"task_entry_id,omitempty"`
	ResultEntryID string `json:"result_entry_id,omitempty"`
	Status        string `json:"status,omitempty"`
	GroupExists   bool   `json:"group_exists"`
	PendingCount  int64  `json:"pending_count"`
}

func main() {
	var (
		redisURL     = flag.String("redis", envDefault("OJOS_REAL_REDIS_URL", "redis://127.0.0.1:6379/0"), "Redis URL")
		mode         = flag.String("mode", "task", "task or result")
		stream       = flag.String("stream", "ojos:judge:task", "task stream")
		resultStream = flag.String("result-stream", "ojos:judge:result", "result stream")
		group        = flag.String("group", "judge-worker", "consumer group")
		submissionID = flag.Int64("submission-id", 0, "submission id")
	)
	flag.Parse()

	if *submissionID <= 0 {
		log.Fatal("submission-id is required")
	}
	options, err := redis.ParseURL(*redisURL)
	if err != nil {
		log.Fatalf("parse redis URL failed: %v", err)
	}
	client := redis.NewClient(options)
	defer client.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := client.Ping(ctx).Err(); err != nil {
		log.Fatalf("ping redis failed: %v", err)
	}

	out := summary{
		Mode:         *mode,
		Stream:       *stream,
		ResultStream: *resultStream,
		Group:        *group,
		SubmissionID: *submissionID,
		TaskID:       fmt.Sprintf("sub-%d", *submissionID),
	}
	out.GroupExists = consumerGroupExists(ctx, client, *stream, *group)
	out.PendingCount = pendingCount(ctx, client, *stream, *group)

	switch strings.ToLower(strings.TrimSpace(*mode)) {
	case "task":
		entryID, taskID, err := findStreamEntry(ctx, client, *stream, *submissionID)
		if err != nil {
			log.Fatal(err)
		}
		out.TaskEntryID = entryID
		out.TaskID = taskID
	case "result":
		entryID, status, err := findResultEntry(ctx, client, *resultStream, *submissionID)
		if err != nil {
			log.Fatal(err)
		}
		out.ResultEntryID = entryID
		out.Status = status
	default:
		log.Fatalf("unsupported mode %q", *mode)
	}

	if err := json.NewEncoder(os.Stdout).Encode(out); err != nil {
		log.Fatal(err)
	}
}

func findStreamEntry(ctx context.Context, client *redis.Client, stream string, submissionID int64) (string, string, error) {
	entries, err := client.XRange(ctx, stream, "-", "+").Result()
	if err != nil {
		return "", "", err
	}
	wantSubmission := strconv.FormatInt(submissionID, 10)
	wantTask := fmt.Sprintf("sub-%d", submissionID)
	for _, entry := range entries {
		if fmt.Sprint(entry.Values["submission_id"]) == wantSubmission && fmt.Sprint(entry.Values["task_id"]) == wantTask {
			return entry.ID, wantTask, nil
		}
	}
	return "", "", fmt.Errorf("task stream entry not found for submission %d", submissionID)
}

func findResultEntry(ctx context.Context, client *redis.Client, stream string, submissionID int64) (string, string, error) {
	entries, err := client.XRange(ctx, stream, "-", "+").Result()
	if err != nil {
		return "", "", err
	}
	wantSubmission := strconv.FormatInt(submissionID, 10)
	for _, entry := range entries {
		if fmt.Sprint(entry.Values["submission_id"]) == wantSubmission {
			return entry.ID, fmt.Sprint(entry.Values["status"]), nil
		}
	}
	return "", "", fmt.Errorf("result stream entry not found for submission %d", submissionID)
}

func consumerGroupExists(ctx context.Context, client *redis.Client, stream string, group string) bool {
	groups, err := client.XInfoGroups(ctx, stream).Result()
	if err != nil {
		return false
	}
	for _, item := range groups {
		if item.Name == group {
			return true
		}
	}
	return false
}

func pendingCount(ctx context.Context, client *redis.Client, stream string, group string) int64 {
	value, err := client.Do(ctx, "XPENDING", stream, group).Result()
	if err != nil {
		return -1
	}
	items, ok := value.([]any)
	if !ok || len(items) == 0 {
		return -1
	}
	switch count := items[0].(type) {
	case int64:
		return count
	case uint64:
		return int64(count)
	default:
		return -1
	}
}

func envDefault(key string, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(key)); value != "" {
		return value
	}
	return fallback
}
