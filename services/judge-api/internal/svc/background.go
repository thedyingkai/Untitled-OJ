// Service-owned background workers and their cancellation scope.
package svc

import (
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	"ojos-judge-api/internal/repository"
	"ojos-problem-events/problemv1"
	"ojos-shared/eventing"
)

func (s *ServiceContext) startProblemProjectionConsumer() error {
	ctx, cancel := context.WithCancel(context.Background())
	s.backgroundCancel = cancel
	var transport eventing.TransportConfig
	if s.Events != nil {
		var err error
		transport, err = s.Events.SubscriberTransport(
			problemv1.DeletedType,
			problemv1.SnapshotType,
		)
		if err != nil {
			// The context was already checked against this exact Release contract
			// at startup; reaching this branch indicates local file corruption.
			return fmt.Errorf("managed Event Contract consumer group is invalid: %w", err)
		}
	} else {
		// Compatibility is intentionally limited to unmanaged development.
		stream := strings.TrimSpace(os.Getenv("OJOS_PROBLEM_EVENT_STREAM"))
		if stream == "" {
			stream = eventing.DefaultEventStream
		}
		group := strings.TrimSpace(os.Getenv("OJOS_PROBLEM_EVENT_CONSUMER_GROUP"))
		if group == "" {
			group = s.Config.Name
		}
		transport = eventing.DevelopmentSubscriberTransport(stream, group)
	}
	hostname, _ := os.Hostname()
	consumer, err := eventing.NewConsumer(s.DB, s.EventRedis, transport, repository.ApplyProblemProjection)
	if err != nil {
		return fmt.Errorf("configure problem projection consumer failed: %w", err)
	}
	consumer.ConsumerName = fmt.Sprintf("%s-%s-%d", s.Config.Name, hostname, os.Getpid())
	consumer.BatchSize = 100
	consumer.ClaimIdle = 30 * time.Second
	consumer.MaxAttempts = eventing.DefaultMaxAttempts
	// Construct every consumer before starting any background work. The service
	// context owns the cancellation and joins both loops before closing stores.
	if s.ResultOutbox != nil {
		s.backgroundWG.Add(1)
		go func() {
			defer s.backgroundWG.Done()
			s.ResultOutbox.Run(ctx)
		}()
	}
	s.backgroundWG.Add(1)
	go func() {
		defer s.backgroundWG.Done()
		consumer.Run(ctx)
	}()
	return nil
}
