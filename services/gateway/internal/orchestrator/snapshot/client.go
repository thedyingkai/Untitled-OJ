package orchestratorsnapshot

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"sort"
	"strings"
	"time"
)

const (
	orchestratorTokenHeader       = "x-ojos-orchestrator-token"
	contributionAckTokenHeader    = "x-ojos-contribution-ack-token"
	contributionAckSchema         = "ojos.dev/contribution-projection-ack/v1"
	contributionAckPath           = "/api/v1/contributions/projections:ack"
	maximumContributionAckBodyLen = 1024 * 1024
)

type Client struct {
	endpoint      string
	internalToken string
	ackToken      string
	httpClient    *http.Client
}

type envelope[T any] struct {
	Code int    `json:"code"`
	Msg  string `json:"msg"`
	Data T      `json:"data"`
}

type v1Envelope[T any] struct {
	Data T `json:"data"`
	Meta struct {
		RequestID  string `json:"request_id"`
		APIVersion string `json:"api_version"`
	} `json:"meta"`
}

type OperationsResponse struct {
	Operations []Operation `json:"operations"`
}

type Operation struct {
	OperationID   string          `json:"operation_id"`
	ServiceID     string          `json:"service_id"`
	Action        string          `json:"action"`
	Status        string          `json:"status"`
	ActorUsername string          `json:"actor_username"`
	Request       json.RawMessage `json:"request"`
	Plan          json.RawMessage `json:"plan"`
	Result        json.RawMessage `json:"result"`
	ErrorMessage  string          `json:"error_message"`
	CreatedAt     string          `json:"created_at"`
	UpdatedAt     string          `json:"updated_at"`
}

type contributionAckRequest struct {
	SchemaVersion    string                        `json:"schema_version"`
	Target           string                        `json:"target"`
	ScopeID          string                        `json:"scope_id"`
	SnapshotDigest   string                        `json:"snapshot_digest"`
	Acknowledgements []ContributionAcknowledgement `json:"acknowledgements"`
}

type contributionAckResponse struct {
	SchemaVersion  string `json:"schema_version"`
	Target         string `json:"target"`
	ScopeID        string `json:"scope_id"`
	SnapshotDigest string `json:"snapshot_digest"`
	Accepted       bool   `json:"accepted"`
}

func NewClient(endpoint string, internalToken string, contributionAckToken ...string) *Client {
	ackToken := ""
	if len(contributionAckToken) > 0 {
		ackToken = contributionAckToken[0]
	}
	return &Client{
		endpoint:      strings.TrimRight(strings.TrimSpace(endpoint), "/"),
		internalToken: strings.TrimSpace(internalToken),
		ackToken:      strings.TrimSpace(ackToken),
		httpClient:    &http.Client{Timeout: 5 * time.Second},
	}
}

func (c *Client) Configured() bool {
	return c != nil && c.endpoint != "" && c.internalToken != ""
}

func (c *Client) ContributionAcknowledgementsConfigured() bool {
	return c != nil && c.Configured() && c.ackToken != ""
}

func (c *Client) DecodeOrchestratorSnapshot(ctx context.Context, includeDisabled bool, out any) error {
	return c.get(ctx, "/internal/orchestrator/snapshot", url.Values{
		"include_disabled": []string{boolString(includeDisabled)},
	}, out)
}

func (c *Client) DecodeOrchestratorRoutes(ctx context.Context, includeDisabled bool, includeUpstream bool, out any) error {
	return c.get(ctx, "/internal/orchestrator/routes", url.Values{
		"include_disabled": []string{boolString(includeDisabled)},
		"include_upstream": []string{boolString(includeUpstream)},
	}, out)
}

func (c *Client) DecodeNodeOrchestratorRoutes(ctx context.Context, nodeID string, includeUpstream bool, out any) error {
	nodeID = strings.TrimSpace(nodeID)
	if nodeID == "" {
		return errors.New("node id is required")
	}
	return c.get(ctx, "/internal/orchestrator/nodes/"+url.PathEscape(nodeID)+"/routes", url.Values{
		"include_upstream": []string{boolString(includeUpstream)},
	}, out)
}

func (c *Client) ContributionSnapshot(ctx context.Context) (ContributionSnapshot, error) {
	var snapshot ContributionSnapshot
	if err := c.get(ctx, "/api/v1/contributions/snapshot", nil, &snapshot); err != nil {
		return ContributionSnapshot{}, err
	}
	if strings.TrimSpace(snapshot.SchemaVersion) != "ojos.dev/contribution-snapshot/v1" {
		return ContributionSnapshot{}, fmt.Errorf("unsupported contribution snapshot schema %q", snapshot.SchemaVersion)
	}
	return snapshot, nil
}

// AcknowledgeContributionSnapshot reports the exact obligations carried by a
// snapshot only after Gateway has atomically installed its routes and frontend
// artifact view. The stable idempotency key makes transport retries harmless.
func (c *Client) AcknowledgeContributionSnapshot(ctx context.Context, snapshot ContributionSnapshot) error {
	if !c.ContributionAcknowledgementsConfigured() {
		return errors.New("contribution acknowledgement client is not configured")
	}
	if strings.TrimSpace(snapshot.ScopeID) == "" || !canonicalSHA256(snapshot.Digest) {
		return errors.New("contribution snapshot acknowledgement identity is invalid")
	}
	requestBody := contributionAckRequest{
		SchemaVersion:    contributionAckSchema,
		Target:           "GATEWAY",
		ScopeID:          snapshot.ScopeID,
		SnapshotDigest:   snapshot.Digest,
		Acknowledgements: append([]ContributionAcknowledgement{}, snapshot.Acknowledgements...),
	}
	body, err := json.Marshal(requestBody)
	if err != nil {
		return fmt.Errorf("encode contribution acknowledgement: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.endpoint+contributionAckPath, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("create contribution acknowledgement: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	req.Header.Set(orchestratorTokenHeader, c.internalToken)
	req.Header.Set(contributionAckTokenHeader, c.ackToken)
	req.Header.Set("Idempotency-Key", "contribution-projection-ack:GATEWAY:"+snapshot.Digest)
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("send contribution acknowledgement: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("contribution acknowledgement returned %s", resp.Status)
	}
	if mediaType := strings.TrimSpace(strings.Split(resp.Header.Get("Content-Type"), ";")[0]); mediaType != "application/json" {
		return fmt.Errorf("contribution acknowledgement returned unsupported Content-Type %q", mediaType)
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, maximumContributionAckBodyLen+1))
	if err != nil {
		return fmt.Errorf("read contribution acknowledgement: %w", err)
	}
	if len(data) > maximumContributionAckBodyLen {
		return errors.New("contribution acknowledgement response exceeds the configured limit")
	}
	var wrapped v1Envelope[contributionAckResponse]
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&wrapped); err != nil {
		return fmt.Errorf("decode contribution acknowledgement: %w", err)
	}
	if err := ensureJSONEOF(decoder); err != nil {
		return err
	}
	ack := wrapped.Data
	if ack.SchemaVersion != contributionAckSchema || ack.Target != "GATEWAY" || ack.ScopeID != snapshot.ScopeID || ack.SnapshotDigest != snapshot.Digest || !ack.Accepted {
		return errors.New("contribution acknowledgement response identity is invalid")
	}
	return nil
}

func (c *Client) ListEndpoints(ctx context.Context) ([]Endpoint, error) {
	snapshot, err := c.snapshotData(ctx, true)
	if err != nil {
		return nil, err
	}
	return snapshot.Endpoints, nil
}

func (c *Client) ListEndpointGroups(ctx context.Context) ([]EndpointGroup, error) {
	endpoints, err := c.ListEndpoints(ctx)
	if err != nil {
		return nil, err
	}
	return EndpointGroups(endpoints), nil
}

func (c *Client) ListServices(ctx context.Context) ([]Service, error) {
	snapshot, err := c.snapshotData(ctx, true)
	if err != nil {
		return nil, err
	}
	return snapshot.ServiceDefinitions, nil
}

func (c *Client) ListPermissions(ctx context.Context) ([]Permission, error) {
	snapshot, err := c.snapshotData(ctx, true)
	if err != nil {
		return nil, err
	}
	return snapshot.Permissions, nil
}

func (c *Client) ListMenus(ctx context.Context) ([]Menu, error) {
	snapshot, err := c.snapshotData(ctx, true)
	if err != nil {
		return nil, err
	}
	return snapshot.Menus, nil
}

func (c *Client) ListFrontendRoutes(ctx context.Context) ([]FrontendRoute, error) {
	snapshot, err := c.snapshotData(ctx, true)
	if err != nil {
		return nil, err
	}
	return snapshot.FrontendRoutes, nil
}

func (c *Client) ListGatewayRoutes(ctx context.Context) ([]GatewayRoute, error) {
	snapshot, err := c.snapshotData(ctx, true)
	if err != nil {
		return nil, err
	}
	return snapshot.GatewayRoutes, nil
}

func (c *Client) ListComponents(ctx context.Context) ([]Component, error) {
	snapshot, err := c.snapshotData(ctx, true)
	if err != nil {
		return nil, err
	}
	return snapshot.Components, nil
}

func (c *Client) ListEdges(ctx context.Context) ([]Edge, error) {
	snapshot, err := c.snapshotData(ctx, true)
	if err != nil {
		return nil, err
	}
	return snapshot.Topology.DependencyEdges, nil
}

func (c *Client) Topology(ctx context.Context) (Topology, error) {
	snapshot, err := c.snapshotData(ctx, true)
	if err != nil {
		return Topology{}, err
	}
	return Topology{
		EndpointGroups: EndpointGroups(snapshot.Endpoints),
		Nodes:          snapshot.ServiceDefinitions,
		Edges:          snapshot.Topology.DependencyEdges,
		Components:     snapshot.Components,
	}, nil
}

func (c *Client) Detail(ctx context.Context, serviceID string) (Detail, error) {
	serviceID = strings.TrimSpace(serviceID)
	snapshot, err := c.snapshotData(ctx, true)
	if err != nil {
		return Detail{}, err
	}
	var detail Detail
	for _, service := range snapshot.ServiceDefinitions {
		if service.ServiceID == serviceID {
			detail.Service = service
			break
		}
	}
	if detail.Service.ServiceID == "" {
		return Detail{}, errors.New("service not found")
	}
	for _, edge := range snapshot.Topology.DependencyEdges {
		if edge.FromServiceID == serviceID {
			detail.Dependencies = append(detail.Dependencies, edge)
		}
		if edge.ToServiceID == serviceID {
			detail.Dependents = append(detail.Dependents, edge)
		}
	}
	for _, item := range snapshot.Components {
		if item.ServiceID == serviceID {
			detail.Components = append(detail.Components, item)
			if item.ComponentType == "health_check" {
				detail.HealthChecks = append(detail.HealthChecks, item)
			}
		}
	}
	for _, item := range snapshot.Permissions {
		if item.ServiceID == serviceID {
			detail.Permissions = append(detail.Permissions, item)
		}
	}
	for _, item := range snapshot.Menus {
		if item.ServiceID == serviceID {
			detail.Menus = append(detail.Menus, item)
		}
	}
	for _, item := range snapshot.FrontendRoutes {
		if item.ServiceID == serviceID {
			detail.FrontendRoutes = append(detail.FrontendRoutes, item)
		}
	}
	for _, item := range snapshot.GatewayRoutes {
		if item.ServiceID == serviceID {
			detail.GatewayRoutes = append(detail.GatewayRoutes, item)
		}
	}
	return detail, nil
}

func (c *Client) ServiceOperations(ctx context.Context, serviceID string) (OperationsResponse, error) {
	var result OperationsResponse
	path := "/internal/operations"
	if strings.TrimSpace(serviceID) != "" {
		path = "/internal/services/" + url.PathEscape(strings.TrimSpace(serviceID)) + "/operations"
	}
	if err := c.get(ctx, path, nil, &result); err != nil {
		return OperationsResponse{}, err
	}
	return result, nil
}

func (c *Client) ServiceOperationDetail(ctx context.Context, operationID string) (OperationsResponse, error) {
	var result OperationsResponse
	if err := c.get(ctx, "/internal/operations/"+url.PathEscape(strings.TrimSpace(operationID)), nil, &result); err != nil {
		return OperationsResponse{}, err
	}
	return result, nil
}

func (c *Client) snapshotData(ctx context.Context, includeDisabled bool) (OrchestratorSnapshotData, error) {
	var snapshot OrchestratorSnapshotData
	if err := c.DecodeOrchestratorSnapshot(ctx, includeDisabled, &snapshot); err != nil {
		return OrchestratorSnapshotData{}, err
	}
	return snapshot, nil
}

func EndpointGroups(endpoints []Endpoint) []EndpointGroup {
	byService := make(map[string][]string)
	for _, endpoint := range endpoints {
		serviceName := strings.TrimSpace(endpoint.ServiceID)
		value := strings.TrimSpace(endpoint.Endpoint)
		if serviceName == "" || value == "" {
			continue
		}
		byService[serviceName] = append(byService[serviceName], value)
	}
	names := make([]string, 0, len(byService))
	for name := range byService {
		names = append(names, name)
	}
	sort.Strings(names)
	groups := make([]EndpointGroup, 0, len(names))
	for _, name := range names {
		items := byService[name]
		sort.Strings(items)
		groups = append(groups, EndpointGroup{
			ServiceName:   name,
			Selector:      name + "[*]",
			EndpointCount: len(items),
			Endpoints:     items,
		})
	}
	return groups
}

func (c *Client) get(ctx context.Context, path string, query url.Values, out any) error {
	if !c.Configured() {
		return errors.New("orchestrator client is not configured")
	}
	target := c.endpoint + path
	if len(query) > 0 {
		target += "?" + query.Encode()
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		return err
	}
	req.Header.Set(orchestratorTokenHeader, c.internalToken)
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode >= http.StatusBadRequest {
		return fmt.Errorf("orchestrator %s returned %s", path, resp.Status)
	}
	var raw json.RawMessage
	if err := json.NewDecoder(resp.Body).Decode(&raw); err != nil {
		return err
	}
	var wrapped envelope[json.RawMessage]
	if err := json.Unmarshal(raw, &wrapped); err == nil && (wrapped.Code != 0 || len(wrapped.Data) > 0 || wrapped.Msg != "") {
		if wrapped.Code != 0 {
			return fmt.Errorf("orchestrator %s failed: %s", path, wrapped.Msg)
		}
		if len(wrapped.Data) == 0 {
			return nil
		}
		return json.Unmarshal(wrapped.Data, out)
	}
	var v1 v1Envelope[json.RawMessage]
	if err := json.Unmarshal(raw, &v1); err == nil && (len(v1.Data) > 0 || v1.Meta.APIVersion != "" || v1.Meta.RequestID != "") {
		if len(v1.Data) == 0 {
			return nil
		}
		return json.Unmarshal(v1.Data, out)
	}
	return json.Unmarshal(raw, out)
}

func canonicalSHA256(value string) bool {
	if len(value) != len("sha256:")+64 || !strings.HasPrefix(value, "sha256:") {
		return false
	}
	for _, character := range value[len("sha256:"):] {
		if !(character >= '0' && character <= '9' || character >= 'a' && character <= 'f') {
			return false
		}
	}
	return true
}

func ensureJSONEOF(decoder *json.Decoder) error {
	var trailing any
	if err := decoder.Decode(&trailing); !errors.Is(err, io.EOF) {
		if err == nil {
			return errors.New("contribution acknowledgement response contains trailing JSON")
		}
		return fmt.Errorf("decode contribution acknowledgement trailer: %w", err)
	}
	return nil
}

func boolString(value bool) string {
	if value {
		return "true"
	}
	return "false"
}
