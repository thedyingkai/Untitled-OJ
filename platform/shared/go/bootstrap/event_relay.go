package bootstrap

import (
	"context"
	"errors"
	"sync"

	"github.com/jackc/pgx/v5/pgxpool"
	"github.com/redis/go-redis/v9"
	"ojos-shared/eventing"
)

const ValueEventRelay = "platform.event-relay"

type EventContextLoader func(string, []string, []eventing.EventSubscription) (*eventing.EventContext, error)

type EventRelayOptions struct {
	Service          string
	PublishTypes     []string
	Subscriptions    []eventing.EventSubscription
	DatabaseValue    string
	RelayValue       string
	RelayID          string
	LoadEventContext EventContextLoader
}

func NewEventRelayFactory(options EventRelayOptions) Factory {
	return FactoryFunc(func(build BuildContext) (Component, error) {
		service := options.Service
		if service == "" {
			service = build.Spec.Name
		}
		if !validToken(service) {
			return nil, errors.New("event relay service identity is invalid")
		}
		databaseValue := defaultValueName(options.DatabaseValue, ValuePostgreSQL)
		pool, err := Resolve[*pgxpool.Pool](build.Values, databaseValue)
		if err != nil {
			return nil, errors.New("event relay database is unavailable")
		}
		loader := options.LoadEventContext
		if loader == nil {
			loader = eventing.LoadEventContextForService
		}
		eventContext, err := loader(service, append([]string(nil), options.PublishTypes...), append([]eventing.EventSubscription(nil), options.Subscriptions...))
		if err != nil {
			return nil, errors.New("load managed event context")
		}
		if eventContext == nil {
			return &ComponentFuncs{}, nil
		}
		client, err := eventContext.RedisClient()
		if err != nil {
			return nil, errors.New("load managed event transport")
		}
		relay, err := eventing.NewRelay(pool, client, eventContext.PublisherTransport())
		if err != nil {
			_ = client.Close()
			return nil, errors.New("configure managed event relay")
		}
		relay.RelayID = options.RelayID
		if relay.RelayID == "" {
			relay.RelayID = service
		}
		name := defaultValueName(options.RelayValue, ValueEventRelay)
		if !validToken(name) {
			_ = client.Close()
			return nil, errors.New("event relay output name is invalid")
		}
		return &eventRelayComponent{client: client, relay: relay, valueName: name}, nil
	})
}

type eventRelayComponent struct {
	client    *redis.Client
	relay     *eventing.Relay
	valueName string

	mu     sync.Mutex
	cancel context.CancelFunc
	done   chan struct{}
}

func (component *eventRelayComponent) Start(ctx context.Context) error {
	if err := component.client.Ping(ctx).Err(); err != nil {
		return errors.New("connect managed event transport")
	}
	relayContext, cancel := context.WithCancel(ctx)
	done := make(chan struct{})
	component.mu.Lock()
	component.cancel = cancel
	component.done = done
	component.mu.Unlock()
	go func() {
		defer close(done)
		component.relay.Run(relayContext)
	}()
	return nil
}

func (component *eventRelayComponent) Close(ctx context.Context) error {
	component.mu.Lock()
	cancel, done := component.cancel, component.done
	component.mu.Unlock()
	if cancel != nil {
		cancel()
	}
	if done != nil {
		select {
		case <-done:
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	if err := component.client.Close(); err != nil {
		return errors.New("close managed event transport")
	}
	return nil
}

func (component *eventRelayComponent) Outputs() map[string]any {
	return map[string]any{component.valueName: component.relay}
}
