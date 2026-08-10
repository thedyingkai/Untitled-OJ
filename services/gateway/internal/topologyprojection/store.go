package topologyprojection

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strings"
	"sync"
	"time"

	"ojos-gateway/internal/orchestrator/servicestatus"
	"ojos-gateway/internal/proxy"
	shared "ojos-shared/topologyprojection"

	"github.com/redis/go-redis/v9"
)

const indexKey = "ojos:gateway:topology-projections:v1"

type Store struct {
	redis *redis.Client
	proxy *proxy.ServiceProxy
	mu    sync.Mutex
}

func NewStore(client *redis.Client, serviceProxy *proxy.ServiceProxy) *Store {
	return &Store{redis: client, proxy: serviceProxy}
}

func (s *Store) Recover(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	documents, err := s.loadAll(ctx)
	if err != nil {
		return err
	}
	s.proxy.SetTopologyRouteTable(routeTable(documents))
	return nil
}

func (s *Store) Get(ctx context.Context, topologyID string) (*shared.Document, error) {
	value, err := s.redis.Get(ctx, documentKey(topologyID)).Bytes()
	if err == redis.Nil {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("load Gateway topology projection: %w", err)
	}
	document, err := shared.DecodeDocument(value)
	if err != nil {
		return nil, fmt.Errorf("decode Gateway topology projection: %w", err)
	}
	return &document, nil
}

func (s *Store) Apply(ctx context.Context, request shared.Request) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	document := request.Document()
	payload, err := json.Marshal(document)
	if err != nil {
		return fmt.Errorf("encode Gateway topology projection: %w", err)
	}
	key := documentKey(request.TopologyID)
	const maxCASAttempts = 8
	for attempt := 0; attempt < maxCASAttempts; attempt++ {
		err = s.redis.Watch(ctx, func(tx *redis.Tx) error {
			var current *shared.Document
			value, getErr := tx.Get(ctx, key).Bytes()
			switch {
			case getErr == nil:
				persisted, decodeErr := shared.DecodeDocument(value)
				if decodeErr != nil {
					return fmt.Errorf("decode locked Gateway topology projection: %w", decodeErr)
				}
				current = &persisted
			case errors.Is(getErr, redis.Nil):
				// An absent key is part of the watched CAS state.
			default:
				return fmt.Errorf("load locked Gateway topology projection: %w", getErr)
			}
			write, transitionErr := shared.PlanApply(current, request)
			if transitionErr != nil {
				return transitionErr
			}
			if !write {
				return nil
			}
			_, persistErr := tx.TxPipelined(ctx, func(pipe redis.Pipeliner) error {
				pipe.Set(ctx, key, payload, 0)
				pipe.SAdd(ctx, indexKey, request.TopologyID)
				return nil
			})
			return persistErr
		}, key)
		if !errors.Is(err, redis.TxFailedErr) {
			break
		}
	}
	if errors.Is(err, redis.TxFailedErr) {
		return fmt.Errorf("persist Gateway topology projection: concurrent projection did not stabilize after %d attempts", maxCASAttempts)
	}
	if err != nil {
		return fmt.Errorf("persist Gateway topology projection: %w", err)
	}
	documents, err := s.loadAll(ctx)
	if err != nil {
		return err
	}
	s.proxy.SetTopologyRouteTable(routeTable(documents))
	return nil
}

func (s *Store) Delete(ctx context.Context, topologyID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	documents, err := s.loadAll(ctx)
	if err != nil {
		return err
	}
	delete(documents, topologyID)
	_, err = s.redis.TxPipelined(ctx, func(pipe redis.Pipeliner) error {
		pipe.Del(ctx, documentKey(topologyID))
		pipe.SRem(ctx, indexKey, topologyID)
		return nil
	})
	if err != nil {
		return fmt.Errorf("delete Gateway topology projection: %w", err)
	}
	s.proxy.SetTopologyRouteTable(routeTable(documents))
	return nil
}

func (s *Store) loadAll(ctx context.Context) (map[string]shared.Document, error) {
	ids, err := s.redis.SMembers(ctx, indexKey).Result()
	if err != nil {
		return nil, fmt.Errorf("list Gateway topology projections: %w", err)
	}
	sort.Strings(ids)
	documents := make(map[string]shared.Document, len(ids))
	for _, id := range ids {
		value, err := s.redis.Get(ctx, documentKey(id)).Bytes()
		if err == redis.Nil {
			// Heal a crash window or old inconsistent index. This mutation does
			// not affect routes and makes subsequent recoveries deterministic.
			_ = s.redis.SRem(ctx, indexKey, id).Err()
			continue
		}
		if err != nil {
			return nil, fmt.Errorf("load Gateway topology %s: %w", id, err)
		}
		document, err := shared.DecodeDocument(value)
		if err != nil {
			return nil, fmt.Errorf("decode Gateway topology %s: %w", id, err)
		}
		if document.Provider != "gateway" || document.TopologyID != id {
			return nil, fmt.Errorf("Gateway topology %s has mismatched durable identity", id)
		}
		documents[id] = document
	}
	return documents, nil
}

func routeTable(documents map[string]shared.Document) servicestatus.RouteTable {
	ids := make([]string, 0, len(documents))
	for id := range documents {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	table := servicestatus.RouteTable{
		Version:     "topology-binding-v1",
		GeneratedAt: time.Now().UTC().Format(time.RFC3339Nano),
		CanProxy:    true,
	}
	seenRequirements := make(map[string]string)
	for _, id := range ids {
		document := documents[id]
		grants := make(map[string]shared.BindingGrant, len(document.Grants))
		for _, grant := range document.Grants {
			grants[grant.BindingID] = grant
		}
		for _, binding := range document.Routes {
			grant, ok := grants[binding.BindingID]
			if !ok || grant.ConsumerDeploymentID != binding.ConsumerDeploymentID ||
				grant.ConsumerServiceID != binding.ConsumerServiceID || grant.ConsumerNodeID != binding.ConsumerNodeID ||
				grant.CredentialGeneration != binding.CredentialGeneration {
				table.Warnings = append(table.Warnings, fmt.Sprintf("binding route %s has no exact identity grant", binding.BindingID))
				continue
			}
			key := binding.ConsumerDeploymentID + "\x00" + binding.RequirementName
			if previous, exists := seenRequirements[key]; exists {
				table.Warnings = append(table.Warnings, fmt.Sprintf("duplicate active binding route %s conflicts with %s", binding.BindingID, previous))
				continue
			}
			seenRequirements[key] = binding.BindingID
			table.Routes = append(table.Routes, servicestatus.ServiceRoute{
				RouteID:              "binding:" + binding.BindingID,
				ApiID:                binding.APIID,
				BindingID:            binding.BindingID,
				ConsumerDeploymentID: binding.ConsumerDeploymentID,
				ConsumerServiceID:    binding.ConsumerServiceID,
				ConsumerNodeID:       binding.ConsumerNodeID,
				CredentialGeneration: binding.CredentialGeneration,
				TimeoutMS:            binding.TimeoutMS,
				ProviderNodeID:       binding.ProviderNodeID,
				ProviderService:      binding.ProviderServiceID,
				ProviderEndpoint:     binding.ProviderEndpoint,
				OwnerServiceID:       "orchestrator",
				Prefix:               binding.VirtualPath,
				ServiceID:            binding.ProviderServiceID,
				TargetService:        binding.ProviderServiceID,
				UpstreamBase:         strings.TrimRight(binding.UpstreamBase, "/"),
				AuthMode:             "workload",
				ProviderAuthMode:     binding.ProviderAuthMode,
				RequiredPermission:   binding.Permission,
				Methods:              append([]string(nil), binding.Methods...),
				Enabled:              true,
				ProxyEnabled:         true,
				Priority:             len(binding.VirtualPath),
				StripPrefix:          binding.VirtualPath,
				RewritePrefix:        binding.ProviderPath,
				CreatedFrom:          "topology_binding_v1",
				Status:               "active",
			})
		}
	}
	sort.Slice(table.Routes, func(i, j int) bool { return table.Routes[i].RouteID < table.Routes[j].RouteID })
	sort.Strings(table.Warnings)
	return table
}

func documentKey(topologyID string) string {
	return "ojos:gateway:topology-projection:v1:" + topologyID
}
