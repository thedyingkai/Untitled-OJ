package eventing

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func writeEventContext(t *testing.T, service string, publishes []string, subscriptions []EventSubscription) string {
	t.Helper()
	root := t.TempDir()
	value := EventContext{
		SchemaVersion: 1,
		Deployment: EventDeployment{
			ID:      "deployment-" + service,
			Service: service,
			Node:    "node-b",
		},
		ConnectionID:   "shared-events",
		ConnectionFile: DefaultEventConnectionFile,
		Stream:         DefaultEventStream,
		PublishTypes:   sortedUnique(publishes),
		Subscriptions:  sortedSubscriptions(subscriptions),
		Generation:     7,
	}
	bytes, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "events.json")
	if err := os.WriteFile(path, bytes, 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestGenericProducerAndConsumerUseOneContractSelectedStream(t *testing.T) {
	t.Setenv("OJOS_ENVIRONMENT", "")
	producerPath := writeEventContext(t, "fixture-producer", []string{"io.example.fixture.v1"}, nil)
	t.Setenv("OJOS_EVENT_CONTEXT_FILE", producerPath)
	producer, err := LoadEventContextForService("fixture-producer", []string{"io.example.fixture.v1"}, nil)
	if err != nil {
		t.Fatal(err)
	}

	consumerPath := writeEventContext(t, "fixture-consumer", nil, []EventSubscription{{
		EventType:     "io.example.fixture.v1",
		ConsumerGroup: "fixture-consumer",
	}})
	t.Setenv("OJOS_EVENT_CONTEXT_FILE", consumerPath)
	consumer, err := LoadEventContextForService("fixture-consumer", nil, []EventSubscription{{
		EventType:     "io.example.fixture.v1",
		ConsumerGroup: "fixture-consumer",
	}})
	if err != nil {
		t.Fatal(err)
	}

	if producer.Stream != consumer.Stream || producer.Stream != "ojos:events:v1" {
		t.Fatalf("service names split the shared stream: producer=%q consumer=%q", producer.Stream, consumer.Stream)
	}
	if producer.ConnectionID != consumer.ConnectionID {
		t.Fatalf("producer and consumer selected different event providers: %q != %q", producer.ConnectionID, consumer.ConnectionID)
	}
	group, err := consumer.ConsumerGroupFor("io.example.fixture.v1")
	if err != nil || group != "fixture-consumer" {
		t.Fatalf("unexpected consumer group %q: %v", group, err)
	}
}

func TestEventContextDeclarationMismatchFailsClosed(t *testing.T) {
	t.Setenv("OJOS_ENVIRONMENT", "")
	path := writeEventContext(t, "fixture-producer", []string{"io.example.deleted.v1", "io.example.snapshot.v1"}, nil)
	t.Setenv("OJOS_EVENT_CONTEXT_FILE", path)
	if _, err := LoadEventContextForService("fixture-producer", []string{"io.example.snapshot.v1"}, nil); err == nil {
		t.Fatal("expected a materialized/Release declaration mismatch to fail")
	}
	if _, err := LoadEventContextForService("fixture-consumer", []string{"io.example.deleted.v1", "io.example.snapshot.v1"}, nil); err == nil {
		t.Fatal("expected a deployment service mismatch to fail")
	}
}

func TestProductionRequiresManagedEventContext(t *testing.T) {
	for _, environment := range []string{"production", "staging"} {
		t.Run(environment, func(t *testing.T) {
			t.Setenv("OJOS_ENVIRONMENT", environment)
			t.Setenv("OJOS_EVENT_CONTEXT_FILE", "")
			t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "")
			if _, err := LoadEventContextForService("fixture", []string{"io.example.v1"}, nil); err == nil {
				t.Fatalf("%s must not fall back to REDIS_URL when events.json is missing", environment)
			}
		})
	}
}

func TestOnlyUnmanagedDevelopmentMayUseLegacyEventEnvironment(t *testing.T) {
	for _, environment := range []string{"", "development", "test"} {
		t.Run("environment-"+environment, func(t *testing.T) {
			t.Setenv("OJOS_ENVIRONMENT", environment)
			t.Setenv("OJOS_EVENT_CONTEXT_FILE", "")
			t.Setenv("OJOS_SERVICE_CONTEXT_FILE", "")
			value, err := LoadEventContextForService("fixture", []string{"io.example.v1"}, nil)
			if err != nil || value != nil {
				t.Fatalf("unmanaged development should keep the legacy fallback, value=%v err=%v", value, err)
			}
		})
	}
}

func TestEventContextRejectsPartialJSONAndMissingLocalConnection(t *testing.T) {
	t.Setenv("OJOS_ENVIRONMENT", "")
	root := t.TempDir()
	partial := filepath.Join(root, "events.json")
	if err := os.WriteFile(partial, []byte(`{"schema_version":1,"deployment":`), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("OJOS_EVENT_CONTEXT_FILE", partial)
	if _, err := LoadEventContextForService("fixture", []string{"io.example.v1"}, nil); err == nil {
		t.Fatal("partial Event Context must never be accepted")
	}

	path := writeEventContext(t, "fixture", []string{"io.example.v1"}, nil)
	t.Setenv("OJOS_EVENT_CONTEXT_FILE", path)
	value, err := LoadEventContextForService("fixture", []string{"io.example.v1"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := value.RedisClient(); err == nil || !strings.Contains(err.Error(), "Agent-local") {
		t.Fatalf("missing Agent-local provider must fail closed, got %v", err)
	}
}
