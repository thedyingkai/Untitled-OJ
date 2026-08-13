package svc

import (
	"context"
	"errors"
	"net"
	"testing"

	"github.com/redis/go-redis/v9"
)

func TestJudgeRedisClientAllowsStartupWhenEndpointIsUnavailable(t *testing.T) {
	client, err := newJudgeRedisClient(nil, "redis://redis.invalid:6379/0")
	if err != nil {
		t.Fatalf("valid Redis configuration was rejected before connectivity could recover: %v", err)
	}
	client.AddHook(unavailableRedisHook{})
	defer client.Close()

	if err := probeJudgeRedis(context.Background(), client); err == nil {
		t.Fatal("fixture did not simulate an unavailable Redis endpoint")
	}
	// The probe is deliberately advisory: NewServiceContext logs this error
	// and retains this client so API startup and PostgreSQL task polling can
	// continue while Redis reconnects on later commands.
	if client == nil {
		t.Fatal("unavailable Redis endpoint discarded the configured client")
	}
}

func TestJudgeRedisClientRejectsInvalidConfiguration(t *testing.T) {
	for _, redisURL := range []string{
		"",
		"not-a-url",
		"http://redis.example:6379",
	} {
		t.Run(redisURL, func(t *testing.T) {
			client, err := newJudgeRedisClient(nil, redisURL)
			if err == nil {
				if client != nil {
					_ = client.Close()
				}
				t.Fatalf("invalid Redis configuration %q was accepted", redisURL)
			}
			if client != nil {
				t.Fatalf("invalid Redis configuration %q returned a client", redisURL)
			}
		})
	}
}

type unavailableRedisHook struct{}

func (unavailableRedisHook) DialHook(redis.DialHook) redis.DialHook {
	return func(context.Context, string, string) (net.Conn, error) {
		return nil, errors.New("redis unavailable")
	}
}

func (unavailableRedisHook) ProcessHook(redis.ProcessHook) redis.ProcessHook {
	return func(context.Context, redis.Cmder) error {
		return errors.New("redis unavailable")
	}
}

func (unavailableRedisHook) ProcessPipelineHook(redis.ProcessPipelineHook) redis.ProcessPipelineHook {
	return func(context.Context, []redis.Cmder) error {
		return errors.New("redis unavailable")
	}
}
