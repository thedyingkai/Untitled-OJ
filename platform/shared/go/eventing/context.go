package eventing

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"sort"
	"strings"

	"ojos-shared/servicecontext"

	"github.com/redis/go-redis/v9"
)

const (
	DefaultEventContextFile    = "/run/ojos/service/events.json"
	DefaultEventConnectionFile = "/run/ojos/service/event-redis.url"
	DefaultEventStream         = "ojos:events:v1"
	maxEventContextBytes       = 1 << 20
	maxEventConnectionBytes    = 64 << 10
)

type EventContext struct {
	SchemaVersion  int                 `json:"schema_version"`
	Deployment     EventDeployment     `json:"deployment"`
	ConnectionID   string              `json:"connection_id"`
	ConnectionFile string              `json:"connection_file"`
	Stream         string              `json:"stream"`
	PublishTypes   []string            `json:"publish_types"`
	Subscriptions  []EventSubscription `json:"subscriptions"`
	Generation     uint64              `json:"generation"`
}

type EventDeployment struct {
	ID      string `json:"id"`
	Service string `json:"service"`
	Node    string `json:"node"`
}

type EventSubscription struct {
	EventType     string `json:"event_type"`
	ConsumerGroup string `json:"consumer_group"`
}

// TransportConfig is an adapter-only view of the Agent materialized broker
// binding. Domain publishers and handlers should depend on Codec/Enqueue and
// never accept stream or consumer-group strings.
type TransportConfig struct {
	stream        string
	consumerGroup string
}

func (value EventContext) PublisherTransport() TransportConfig {
	return TransportConfig{stream: value.Stream}
}

func (value EventContext) SubscriberTransport(eventTypes ...string) (TransportConfig, error) {
	group, err := value.ConsumerGroupFor(eventTypes...)
	if err != nil {
		return TransportConfig{}, err
	}
	return TransportConfig{stream: value.Stream, consumerGroup: group}, nil
}

// DevelopmentPublisherTransport is the unmanaged-development escape hatch.
// Production adapters must use the Agent materialized EventContext.
func DevelopmentPublisherTransport(stream string) TransportConfig {
	return TransportConfig{stream: strings.TrimSpace(stream)}
}

// DevelopmentSubscriberTransport is the unmanaged-development/test escape
// hatch. Broker names remain confined to bootstrap and transport tests.
func DevelopmentSubscriberTransport(stream, consumerGroup string) TransportConfig {
	return TransportConfig{stream: strings.TrimSpace(stream), consumerGroup: strings.TrimSpace(consumerGroup)}
}

// LoadEventContextForService loads the Agent-materialized Event Contract. It
// returns nil only for an unmanaged development process. Production and any
// process with a Service Context fail closed when events.json is missing.
func LoadEventContextForService(
	expectedService string,
	expectedPublishes []string,
	expectedSubscriptions []EventSubscription,
) (*EventContext, error) {
	explicit := strings.TrimSpace(os.Getenv("OJOS_EVENT_CONTEXT_FILE"))
	path := explicit
	if path == "" {
		path = DefaultEventContextFile
	}
	_, err := os.Stat(path)
	if errors.Is(err, os.ErrNotExist) {
		managed, managedErr := servicecontext.LoadOptional()
		if managedErr != nil {
			return nil, managedErr
		}
		environment := strings.ToLower(strings.TrimSpace(os.Getenv("OJOS_ENVIRONMENT")))
		unmanagedDevelopment := environment == "" || environment == "development" || environment == "test"
		if explicit != "" || managed != nil || envEnabled("OJOS_MANAGED_WORKLOAD") || !unmanagedDevelopment {
			return nil, fmt.Errorf("managed Event Contract context is required but missing: %s", path)
		}
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("inspect Event Contract context: %w", err)
	}
	value, err := loadEventContext(path)
	if err != nil {
		return nil, err
	}
	if value.Deployment.Service != strings.TrimSpace(expectedService) {
		return nil, fmt.Errorf("event context belongs to %s, expected %s", value.Deployment.Service, expectedService)
	}
	expectedPublishes = sortedUnique(expectedPublishes)
	expectedSubscriptions = sortedSubscriptions(expectedSubscriptions)
	if !equalStrings(value.PublishTypes, expectedPublishes) || !equalSubscriptions(value.Subscriptions, expectedSubscriptions) {
		return nil, errors.New("materialized event context does not match the Release Event Contract")
	}
	return &value, nil
}

func loadEventContext(path string) (EventContext, error) {
	info, err := os.Stat(path)
	if err != nil {
		return EventContext{}, err
	}
	if !info.Mode().IsRegular() || info.Size() <= 0 || info.Size() > maxEventContextBytes {
		return EventContext{}, errors.New("event context must be a bounded regular file")
	}
	file, err := os.Open(path)
	if err != nil {
		return EventContext{}, err
	}
	defer file.Close()
	decoder := json.NewDecoder(io.LimitReader(file, maxEventContextBytes+1))
	decoder.DisallowUnknownFields()
	var value EventContext
	if err := decoder.Decode(&value); err != nil {
		return EventContext{}, fmt.Errorf("decode event context: %w", err)
	}
	var trailing any
	if err := decoder.Decode(&trailing); !errors.Is(err, io.EOF) {
		return EventContext{}, errors.New("event context contains trailing JSON")
	}
	if err := value.Validate(); err != nil {
		return EventContext{}, err
	}
	return value, nil
}

func (value EventContext) Validate() error {
	if value.SchemaVersion != 1 || value.Generation == 0 {
		return errors.New("unsupported event context schema/generation")
	}
	for name, field := range map[string]string{
		"deployment.id": value.Deployment.ID, "deployment.service": value.Deployment.Service,
		"deployment.node": value.Deployment.Node, "connection_id": value.ConnectionID,
		"stream": value.Stream,
	} {
		if strings.TrimSpace(field) == "" || len(field) > 256 || strings.IndexFunc(field, func(r rune) bool { return r < 0x21 || r == 0x7f }) >= 0 {
			return fmt.Errorf("event context %s is invalid", name)
		}
	}
	if value.ConnectionFile != DefaultEventConnectionFile {
		return fmt.Errorf("event context connection_file must be %s", DefaultEventConnectionFile)
	}
	if value.Stream != DefaultEventStream {
		return fmt.Errorf("event context stream must be %s", DefaultEventStream)
	}
	if len(value.PublishTypes) == 0 && len(value.Subscriptions) == 0 {
		return errors.New("event context declares no events")
	}
	if !equalStrings(value.PublishTypes, sortedUnique(value.PublishTypes)) || !equalSubscriptions(value.Subscriptions, sortedSubscriptions(value.Subscriptions)) {
		return errors.New("event context declarations must be sorted and unique")
	}
	for _, subscription := range value.Subscriptions {
		if strings.TrimSpace(subscription.EventType) == "" || strings.TrimSpace(subscription.ConsumerGroup) == "" {
			return errors.New("event context subscription is incomplete")
		}
	}
	return nil
}

func (value EventContext) RedisClient() (*redis.Client, error) {
	info, err := os.Stat(value.ConnectionFile)
	if err != nil {
		return nil, fmt.Errorf("inspect Agent-local event connection: %w", err)
	}
	if !info.Mode().IsRegular() || info.Size() <= 0 || info.Size() > maxEventConnectionBytes {
		return nil, errors.New("Agent-local event connection file is invalid")
	}
	bytes, err := os.ReadFile(value.ConnectionFile)
	if err != nil {
		return nil, fmt.Errorf("read Agent-local event connection: %w", err)
	}
	connection := strings.TrimSpace(string(bytes))
	if connection == "" || strings.IndexFunc(connection, func(r rune) bool { return r == '\r' || r == '\n' || r == '\t' || r == ' ' }) >= 0 {
		return nil, errors.New("Agent-local event connection is invalid")
	}
	options, err := redis.ParseURL(connection)
	if err != nil {
		return nil, errors.New("Agent-local event connection is not a Redis URL")
	}
	return redis.NewClient(options), nil
}

func (value EventContext) ConsumerGroupFor(eventTypes ...string) (string, error) {
	group := ""
	for _, eventType := range eventTypes {
		found := ""
		for _, subscription := range value.Subscriptions {
			if subscription.EventType == eventType {
				found = subscription.ConsumerGroup
				break
			}
		}
		if found == "" {
			return "", fmt.Errorf("event type %s is not subscribed by this deployment", eventType)
		}
		if group != "" && group != found {
			return "", errors.New("requested event types use different consumer groups")
		}
		group = found
	}
	return group, nil
}

func sortedUnique(values []string) []string {
	result := append([]string(nil), values...)
	sort.Strings(result)
	result = compactStrings(result)
	return result
}

func compactStrings(values []string) []string {
	if len(values) == 0 {
		return values
	}
	result := values[:1]
	for _, value := range values[1:] {
		if value != result[len(result)-1] {
			result = append(result, value)
		}
	}
	return result
}

func sortedSubscriptions(values []EventSubscription) []EventSubscription {
	result := append([]EventSubscription(nil), values...)
	sort.Slice(result, func(i, j int) bool {
		if result[i].EventType == result[j].EventType {
			return result[i].ConsumerGroup < result[j].ConsumerGroup
		}
		return result[i].EventType < result[j].EventType
	})
	if len(result) < 2 {
		return result
	}
	compacted := result[:1]
	for _, value := range result[1:] {
		if value != compacted[len(compacted)-1] {
			compacted = append(compacted, value)
		}
	}
	return compacted
}

func equalStrings(left, right []string) bool {
	if len(left) != len(right) {
		return false
	}
	for index := range left {
		if left[index] != right[index] {
			return false
		}
	}
	return true
}

func equalSubscriptions(left, right []EventSubscription) bool {
	if len(left) != len(right) {
		return false
	}
	for index := range left {
		if left[index] != right[index] {
			return false
		}
	}
	return true
}

func envEnabled(name string) bool {
	value := strings.TrimSpace(os.Getenv(name))
	return value == "1" || strings.EqualFold(value, "true")
}
