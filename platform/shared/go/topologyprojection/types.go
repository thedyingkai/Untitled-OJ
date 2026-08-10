// Package topologyprojection defines the synchronous v1 management contract
// used by the Orchestrator to project one immutable topology revision into the
// Gateway and Auth services.  The desired route/grant rows are computed by the
// control plane; providers persist and expose exactly that projection rather
// than attempting to resolve services from the topology graph themselves.
package topologyprojection

import (
	"bytes"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/url"
	"reflect"
	"sort"
	"strings"
	"time"
)

const APIVersion = "v1"

type Request struct {
	APIVersion           string          `json:"api_version"`
	Provider             string          `json:"provider"`
	Action               string          `json:"action"`
	TopologyID           string          `json:"topology_id"`
	AttemptedRevisionID  string          `json:"attempted_revision_id"`
	DesiredRevisionID    *string         `json:"desired_revision_id"`
	DesiredContentSHA256 *string         `json:"desired_content_sha256"`
	OperationID          string          `json:"operation_id"`
	Spec                 json.RawMessage `json:"spec"`
	Routes               []BindingRoute  `json:"routes"`
	Grants               []BindingGrant  `json:"grants"`
}

type BindingRoute struct {
	BindingID            string   `json:"binding_id"`
	RequirementName      string   `json:"requirement_name"`
	ConsumerDeploymentID string   `json:"consumer_deployment_id"`
	ConsumerServiceID    string   `json:"consumer_service_id"`
	ConsumerNodeID       string   `json:"consumer_node_id"`
	CredentialGeneration uint64   `json:"credential_generation"`
	APIID                string   `json:"api_id"`
	ProviderDeploymentID string   `json:"provider_deployment_id"`
	ProviderServiceID    string   `json:"provider_service_id"`
	ProviderNodeID       string   `json:"provider_node_id"`
	ProviderEndpoint     string   `json:"provider_endpoint"`
	UpstreamBase         string   `json:"upstream_base"`
	ProviderPath         string   `json:"provider_path"`
	VirtualPath          string   `json:"virtual_path"`
	AuthMode             string   `json:"auth_mode"`
	ProviderAuthMode     string   `json:"provider_auth_mode"`
	Permission           string   `json:"permission"`
	Methods              []string `json:"methods"`
	TimeoutMS            uint64   `json:"timeout_ms"`
}

type BindingGrant struct {
	BindingID            string `json:"binding_id"`
	RequirementName      string `json:"requirement_name"`
	ConsumerDeploymentID string `json:"consumer_deployment_id"`
	ConsumerServiceID    string `json:"consumer_service_id"`
	ConsumerNodeID       string `json:"consumer_node_id"`
	CredentialGeneration uint64 `json:"credential_generation"`
	APIID                string `json:"api_id"`
	Permission           string `json:"permission"`
}

type Document struct {
	Provider      string          `json:"provider"`
	TopologyID    string          `json:"topology_id"`
	RevisionID    string          `json:"revision_id"`
	ContentSHA256 string          `json:"content_sha256"`
	OperationID   string          `json:"operation_id"`
	Spec          json.RawMessage `json:"spec"`
	Routes        []BindingRoute  `json:"routes"`
	Grants        []BindingGrant  `json:"grants"`
	UpdatedAt     string          `json:"updated_at"`
}

type Ack struct {
	APIVersion            string  `json:"api_version"`
	Provider              string  `json:"provider"`
	Action                string  `json:"action"`
	TopologyID            string  `json:"topology_id"`
	OperationID           string  `json:"operation_id"`
	Completed             bool    `json:"completed"`
	ObservedRevisionID    *string `json:"observed_revision_id"`
	ObservedContentSHA256 *string `json:"observed_content_sha256"`
	Absent                bool    `json:"absent"`
}

type Status struct {
	APIVersion               string           `json:"api_version"`
	Provider                 string           `json:"provider"`
	TopologyID               string           `json:"topology_id"`
	ObservedRevisionID       *string          `json:"observed_revision_id"`
	ObservedContentSHA256    *string          `json:"observed_content_sha256"`
	ObservedProjectionSHA256 *string          `json:"observed_projection_sha256,omitempty"`
	Absent                   bool             `json:"absent"`
	Endpoints                []EndpointStatus `json:"endpoints"`
	Links                    []LinkStatus     `json:"links"`
}

type EndpointStatus struct {
	Endpoint   string  `json:"endpoint"`
	Health     string  `json:"health"`
	Reachable  bool    `json:"reachable"`
	LatencyMS  *uint64 `json:"latency_ms,omitempty"`
	Message    string  `json:"message"`
	ObservedAt string  `json:"observed_at"`
}

type LinkStatus struct {
	SourceEndpoint string  `json:"source_endpoint"`
	TargetEndpoint string  `json:"target_endpoint"`
	Health         string  `json:"health"`
	LatencyMS      *uint64 `json:"latency_ms,omitempty"`
	Message        string  `json:"message"`
	ObservedAt     string  `json:"observed_at"`
}

type topologySpec struct {
	TopologyID string `json:"topology_id"`
	Endpoints  []struct {
		Endpoint string `json:"endpoint"`
	} `json:"endpoints"`
	Links []struct {
		SourceEndpoint string `json:"source_endpoint"`
		TargetEndpoint string `json:"target_endpoint"`
		Enabled        bool   `json:"enabled"`
	} `json:"links"`
}

func (r *Request) Validate(expectedProvider, pathTopologyID string) error {
	if r.APIVersion != APIVersion || r.Provider != expectedProvider || r.TopologyID != pathTopologyID {
		return errors.New("provider request identity does not match the resource")
	}
	if !validToken(r.TopologyID) || !validToken(r.AttemptedRevisionID) || !validToken(r.OperationID) {
		return errors.New("topology, revision and operation identifiers are required")
	}
	if r.Action != "apply" && r.Action != "restore_previous" && r.Action != "delete" {
		return fmt.Errorf("unsupported topology action %q", r.Action)
	}
	specJSON := strings.TrimSpace(string(r.Spec))
	hasSpec := specJSON != "" && specJSON != "null"
	if r.Action == "delete" {
		if r.DesiredRevisionID != nil || r.DesiredContentSHA256 != nil || hasSpec || len(r.Routes) != 0 || len(r.Grants) != 0 {
			return errors.New("delete request must describe an absent projection")
		}
		return nil
	}
	if r.DesiredRevisionID == nil || !validToken(*r.DesiredRevisionID) || r.DesiredContentSHA256 == nil || !validSHA256(*r.DesiredContentSHA256) || !hasSpec {
		return errors.New("apply/restore requires revision, content hash and topology spec")
	}
	if r.Action == "apply" && r.AttemptedRevisionID != *r.DesiredRevisionID {
		return errors.New("apply desired revision must equal the attempted revision")
	}
	if r.Action == "restore_previous" && r.AttemptedRevisionID == *r.DesiredRevisionID {
		return errors.New("restore desired revision must differ from the attempted revision")
	}
	var spec topologySpec
	if err := json.Unmarshal(r.Spec, &spec); err != nil || spec.TopologyID != r.TopologyID {
		return errors.New("topology spec is invalid or belongs to another resource")
	}
	routesByBindingID := make(map[string]BindingRoute, len(r.Routes))
	consumerRequirements := make(map[string]bool, len(r.Routes))
	for i := range r.Routes {
		if err := r.Routes[i].Validate(); err != nil {
			return fmt.Errorf("route %d: %w", i, err)
		}
		if _, exists := routesByBindingID[r.Routes[i].BindingID]; exists {
			return fmt.Errorf("duplicate route binding_id %s", r.Routes[i].BindingID)
		}
		routesByBindingID[r.Routes[i].BindingID] = r.Routes[i]
		key := r.Routes[i].ConsumerDeploymentID + "\x00" + r.Routes[i].RequirementName
		if consumerRequirements[key] {
			return fmt.Errorf("consumer %s has multiple routes for requirement %s", r.Routes[i].ConsumerDeploymentID, r.Routes[i].RequirementName)
		}
		consumerRequirements[key] = true
	}
	grantIDs := make(map[string]bool, len(r.Grants))
	for i := range r.Grants {
		if err := r.Grants[i].Validate(); err != nil {
			return fmt.Errorf("grant %d: %w", i, err)
		}
		if grantIDs[r.Grants[i].BindingID] {
			return fmt.Errorf("duplicate grant binding_id %s", r.Grants[i].BindingID)
		}
		grantIDs[r.Grants[i].BindingID] = true
		route, exists := routesByBindingID[r.Grants[i].BindingID]
		if !exists {
			return fmt.Errorf("grant %s has no corresponding route", r.Grants[i].BindingID)
		}
		grant := r.Grants[i]
		if route.RequirementName != grant.RequirementName ||
			route.ConsumerDeploymentID != grant.ConsumerDeploymentID ||
			route.ConsumerServiceID != grant.ConsumerServiceID ||
			route.ConsumerNodeID != grant.ConsumerNodeID ||
			route.CredentialGeneration != grant.CredentialGeneration ||
			route.APIID != grant.APIID ||
			route.Permission != grant.Permission {
			return fmt.Errorf("grant %s does not exactly match its route identity", grant.BindingID)
		}
	}
	if len(routesByBindingID) != len(grantIDs) {
		return errors.New("every route must have one exact grant")
	}
	return nil
}

// PlanApply performs the provider-side compare-and-swap check for an apply or
// restore_previous request. The returned boolean is true only when the caller
// must persist request.Document(). A false result with no error is an exact
// idempotent replay and must not rewrite the durable projection.
//
// A restore is deliberately narrower than a normal apply: the provider only
// accepts the same Operation that installed the attempted revision, and only
// while that exact attempted revision is still current. Once restored, only
// an exact replay of the already-persisted desired projection is accepted.
// This prevents a delayed compensation from overwriting a newer Operation.
func PlanApply(current *Document, request Request) (bool, error) {
	desired := request.Document()
	if current == nil {
		if request.Action == "restore_previous" {
			return false, fmt.Errorf("operation %s cannot restore absent topology %s", request.OperationID, request.TopologyID)
		}
		return true, nil
	}
	if current.Provider != request.Provider || current.TopologyID != request.TopologyID {
		return false, errors.New("persisted topology projection identity does not match the request")
	}
	if sameDocumentState(*current, desired) {
		return false, nil
	}
	if request.Action == "restore_previous" {
		if current.OperationID != request.OperationID || current.RevisionID != request.AttemptedRevisionID {
			return false, fmt.Errorf(
				"operation %s cannot restore topology %s because attempted revision %s is not current",
				request.OperationID,
				request.TopologyID,
				request.AttemptedRevisionID,
			)
		}
		return true, nil
	}
	if current.OperationID == request.OperationID {
		return false, fmt.Errorf("operation %s was already used for another projection", request.OperationID)
	}
	return true, nil
}

func sameDocumentState(current, desired Document) bool {
	return current.Provider == desired.Provider &&
		current.TopologyID == desired.TopologyID &&
		current.RevisionID == desired.RevisionID &&
		current.ContentSHA256 == desired.ContentSHA256 &&
		current.OperationID == desired.OperationID &&
		equalJSON(current.Spec, desired.Spec) &&
		reflect.DeepEqual(current.Routes, desired.Routes) &&
		reflect.DeepEqual(current.Grants, desired.Grants)
}

func equalJSON(left, right json.RawMessage) bool {
	if bytes.Equal(left, right) {
		return true
	}
	decode := func(data json.RawMessage) (any, error) {
		decoder := json.NewDecoder(bytes.NewReader(data))
		decoder.UseNumber()
		var value any
		if err := decoder.Decode(&value); err != nil {
			return nil, err
		}
		return value, nil
	}
	leftValue, leftErr := decode(left)
	rightValue, rightErr := decode(right)
	return leftErr == nil && rightErr == nil && reflect.DeepEqual(leftValue, rightValue)
}

func (r BindingRoute) Validate() error {
	for name, value := range map[string]string{
		"binding_id": r.BindingID, "requirement_name": r.RequirementName, "consumer_deployment_id": r.ConsumerDeploymentID,
		"consumer_service_id": r.ConsumerServiceID, "consumer_node_id": r.ConsumerNodeID,
		"api_id": r.APIID, "provider_deployment_id": r.ProviderDeploymentID,
		"provider_service_id": r.ProviderServiceID, "provider_node_id": r.ProviderNodeID,
		"provider_endpoint": r.ProviderEndpoint, "provider_path": r.ProviderPath,
		"virtual_path": r.VirtualPath, "permission": r.Permission,
	} {
		if !validToken(value) {
			return fmt.Errorf("%s is required", name)
		}
	}
	if r.CredentialGeneration == 0 || r.TimeoutMS == 0 {
		return errors.New("credential_generation and timeout_ms must be positive")
	}
	if r.AuthMode != "workload" {
		return errors.New("binding route auth_mode must be workload")
	}
	if r.ProviderAuthMode != "workload" && r.ProviderAuthMode != "public" {
		return errors.New("binding route provider_auth_mode must be workload or public")
	}
	if !strings.HasPrefix(r.ProviderPath, "/") || !strings.HasPrefix(r.VirtualPath, "/internal/apis/") {
		return errors.New("binding route paths are invalid")
	}
	u, err := url.Parse(r.UpstreamBase)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" {
		return errors.New("upstream_base must be an http(s) origin/base URL")
	}
	if len(r.Methods) == 0 {
		return errors.New("binding route methods are required")
	}
	for _, method := range r.Methods {
		if !validMethod(method) {
			return fmt.Errorf("invalid method %q", method)
		}
	}
	return nil
}

func (g BindingGrant) Validate() error {
	for name, value := range map[string]string{
		"binding_id": g.BindingID, "requirement_name": g.RequirementName, "consumer_deployment_id": g.ConsumerDeploymentID,
		"consumer_service_id": g.ConsumerServiceID, "consumer_node_id": g.ConsumerNodeID,
		"api_id": g.APIID, "permission": g.Permission,
	} {
		if !validToken(value) {
			return fmt.Errorf("%s is required", name)
		}
	}
	if g.CredentialGeneration == 0 {
		return errors.New("credential_generation must be positive")
	}
	return nil
}

func (r Request) Document() Document {
	routes := append([]BindingRoute(nil), r.Routes...)
	grants := append([]BindingGrant(nil), r.Grants...)
	sort.Slice(routes, func(i, j int) bool { return routes[i].BindingID < routes[j].BindingID })
	sort.Slice(grants, func(i, j int) bool { return grants[i].BindingID < grants[j].BindingID })
	return Document{
		Provider: r.Provider, TopologyID: r.TopologyID, RevisionID: deref(r.DesiredRevisionID),
		ContentSHA256: deref(r.DesiredContentSHA256), OperationID: r.OperationID,
		Spec: append(json.RawMessage(nil), r.Spec...), Routes: routes, Grants: grants,
		UpdatedAt: time.Now().UTC().Format(time.RFC3339Nano),
	}
}

// CanonicalEffectiveProjectionJSON returns the provider projection in the
// exact canonical wire form used to prove that the effective Gateway routes
// and Auth grants still match the control plane's desired projection.
//
// The top-level order is routes then grants. Both collections are ordered by
// binding_id, matching the durable Document representation. Binding validation
// makes that key unique; SliceStable also keeps this helper deterministic for
// legacy documents that predate the uniqueness constraint. Fields inside each
// item retain their declared JSON order and method order because both are part
// of the projected wire state.
func CanonicalEffectiveProjectionJSON(routes []BindingRoute, grants []BindingGrant) ([]byte, error) {
	canonicalRoutes := append([]BindingRoute{}, routes...)
	canonicalGrants := append([]BindingGrant{}, grants...)
	sort.SliceStable(canonicalRoutes, func(i, j int) bool {
		return canonicalRoutes[i].BindingID < canonicalRoutes[j].BindingID
	})
	sort.SliceStable(canonicalGrants, func(i, j int) bool {
		return canonicalGrants[i].BindingID < canonicalGrants[j].BindingID
	})
	return json.Marshal(struct {
		Routes []BindingRoute `json:"routes"`
		Grants []BindingGrant `json:"grants"`
	}{Routes: canonicalRoutes, Grants: canonicalGrants})
}

// EffectiveProjectionSHA256 returns the lowercase, prefix-free SHA-256 of the
// canonical effective projection JSON.
func EffectiveProjectionSHA256(routes []BindingRoute, grants []BindingGrant) (string, error) {
	payload, err := CanonicalEffectiveProjectionJSON(routes, grants)
	if err != nil {
		return "", err
	}
	digest := sha256.Sum256(payload)
	return fmt.Sprintf("%x", digest), nil
}

func (d Document) Status() (Status, error) {
	var spec topologySpec
	if err := json.Unmarshal(d.Spec, &spec); err != nil {
		return Status{}, fmt.Errorf("decode persisted topology spec: %w", err)
	}
	projectionSHA256, err := EffectiveProjectionSHA256(d.Routes, d.Grants)
	if err != nil {
		return Status{}, fmt.Errorf("digest persisted topology projection: %w", err)
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	// Keep collection fields as JSON arrays even when the projection has no
	// endpoints or links.  The management contract is consumed by strict typed
	// clients (including the Rust control plane), where JSON null is not a valid
	// Vec value and must not turn a successfully persisted projection into
	// unreachable provider evidence.
	status := Status{
		APIVersion: APIVersion, Provider: d.Provider, TopologyID: d.TopologyID,
		ObservedRevisionID: ptr(d.RevisionID), ObservedContentSHA256: ptr(d.ContentSHA256),
		ObservedProjectionSHA256: ptr(projectionSHA256),
		Endpoints:                []EndpointStatus{}, Links: []LinkStatus{},
	}
	for _, endpoint := range spec.Endpoints {
		status.Endpoints = append(status.Endpoints, EndpointStatus{Endpoint: endpoint.Endpoint, Health: "UNKNOWN", Reachable: false, Message: "provider projection present", ObservedAt: now})
	}
	for _, link := range spec.Links {
		if link.Enabled {
			status.Links = append(status.Links, LinkStatus{SourceEndpoint: link.SourceEndpoint, TargetEndpoint: link.TargetEndpoint, Health: "UNKNOWN", Message: "provider projection present", ObservedAt: now})
		}
	}
	sort.Slice(status.Endpoints, func(i, j int) bool { return status.Endpoints[i].Endpoint < status.Endpoints[j].Endpoint })
	sort.Slice(status.Links, func(i, j int) bool {
		if status.Links[i].SourceEndpoint == status.Links[j].SourceEndpoint {
			return status.Links[i].TargetEndpoint < status.Links[j].TargetEndpoint
		}
		return status.Links[i].SourceEndpoint < status.Links[j].SourceEndpoint
	})
	return status, nil
}

func AbsentStatus(provider, topologyID string) Status {
	return Status{APIVersion: APIVersion, Provider: provider, TopologyID: topologyID, Absent: true, Endpoints: []EndpointStatus{}, Links: []LinkStatus{}}
}

func AckFor(req Request, absent bool) Ack {
	ack := Ack{APIVersion: APIVersion, Provider: req.Provider, Action: req.Action, TopologyID: req.TopologyID, OperationID: req.OperationID, Completed: true, Absent: absent}
	if !absent {
		ack.ObservedRevisionID = req.DesiredRevisionID
		ack.ObservedContentSHA256 = req.DesiredContentSHA256
	}
	return ack
}

func DecodeRequest(data []byte) (Request, error) {
	var request Request
	if err := decodeStrict(data, &request); err != nil {
		return Request{}, err
	}
	return request, nil
}

func DecodeDocument(data []byte) (Document, error) {
	var document Document
	if err := decodeStrict(data, &document); err != nil {
		return Document{}, err
	}
	return document, nil
}

func decodeStrict(data []byte, target any) error {
	decoder := json.NewDecoder(strings.NewReader(string(data)))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(target); err != nil {
		return err
	}
	var trailing any
	if err := decoder.Decode(&trailing); err != io.EOF {
		return errors.New("trailing JSON value")
	}
	return nil
}

func validToken(value string) bool {
	value = strings.TrimSpace(value)
	if value == "" || len(value) > 512 {
		return false
	}
	for _, r := range value {
		if r < 0x21 || r == 0x7f {
			return false
		}
	}
	return true
}

func validSHA256(value string) bool {
	if len(value) != 64 {
		return false
	}
	for _, r := range value {
		if !(r >= '0' && r <= '9') && !(r >= 'a' && r <= 'f') {
			return false
		}
	}
	return true
}

func validMethod(value string) bool {
	if value == "*" || value == "ANY" {
		return true
	}
	for _, r := range value {
		if r < 'A' || r > 'Z' {
			return false
		}
	}
	return value != ""
}

func deref(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}
func ptr(value string) *string { return &value }
