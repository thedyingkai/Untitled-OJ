package topologyprojection

import (
	"bytes"
	"encoding/json"
	"regexp"
	"testing"
)

func validRequest(provider string) Request {
	revision := "revision-2"
	hash := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	return Request{
		APIVersion: APIVersion, Provider: provider, Action: "apply",
		TopologyID: "primary", AttemptedRevisionID: revision,
		DesiredRevisionID: &revision, DesiredContentSHA256: &hash, OperationID: "operation-1",
		Spec: json.RawMessage(`{"topology_id":"primary","endpoints":[],"links":[]}`),
		Routes: []BindingRoute{{
			BindingID: "binding-1", RequirementName: "storage_get",
			ConsumerDeploymentID: "worker-b", ConsumerServiceID: "judge-worker",
			ConsumerNodeID: "node-b", CredentialGeneration: 3,
			APIID: "storage.object.get", ProviderDeploymentID: "storage-a",
			ProviderServiceID: "storage", ProviderNodeID: "node-a",
			ProviderEndpoint: "10.0.0.1:8080:storage", UpstreamBase: "https://10.0.0.1:8080",
			ProviderPath: "/objects", VirtualPath: "/internal/apis/storage.object.get",
			AuthMode: "workload", ProviderAuthMode: "workload", Permission: "storage.object.read", Methods: []string{"GET"}, TimeoutMS: 300000,
		}},
		Grants: []BindingGrant{{
			BindingID: "binding-1", RequirementName: "storage_get",
			ConsumerDeploymentID: "worker-b", ConsumerServiceID: "judge-worker",
			ConsumerNodeID: "node-b", CredentialGeneration: 3,
			APIID: "storage.object.get", Permission: "storage.object.read",
		}},
	}
}

func TestProjectionRequiresExactScopedRouteAndGrant(t *testing.T) {
	request := validRequest("gateway")
	if err := request.Validate("gateway", "primary"); err != nil {
		t.Fatalf("valid request rejected: %v", err)
	}
	mutations := map[string]func(*BindingGrant){
		"consumer deployment": func(grant *BindingGrant) { grant.ConsumerDeploymentID = "another-consumer" },
		"consumer service":    func(grant *BindingGrant) { grant.ConsumerServiceID = "another-service" },
		"consumer node":       func(grant *BindingGrant) { grant.ConsumerNodeID = "another-node" },
		"requirement":         func(grant *BindingGrant) { grant.RequirementName = "storage_other" },
		"credential generation": func(grant *BindingGrant) {
			grant.CredentialGeneration++
		},
		"api":        func(grant *BindingGrant) { grant.APIID = "storage.object.head" },
		"permission": func(grant *BindingGrant) { grant.Permission = "storage.object.write" },
	}
	for name, mutate := range mutations {
		t.Run(name, func(t *testing.T) {
			mismatched := validRequest("gateway")
			mutate(&mismatched.Grants[0])
			if err := mismatched.Validate("gateway", "primary"); err == nil {
				t.Fatal("mismatched route/grant identity was accepted")
			}
		})
	}
}

func TestDecodeRejectsUnknownManagementFields(t *testing.T) {
	payload, err := json.Marshal(validRequest("auth"))
	if err != nil {
		t.Fatal(err)
	}
	payload = append(payload[:len(payload)-1], []byte(`,"admin_token":"forbidden"}`)...)
	if _, err := DecodeRequest(payload); err == nil {
		t.Fatal("unknown management credential field was accepted")
	}
}

func TestDocumentStatusSerializesEmptyCollectionsAsArrays(t *testing.T) {
	request := validRequest("gateway")
	request.Routes = []BindingRoute{}
	request.Grants = []BindingGrant{}
	request.Spec = json.RawMessage(`{"topology_id":"primary","endpoints":[],"links":[]}`)

	status, err := request.Document().Status()
	if err != nil {
		t.Fatal(err)
	}
	payload, err := json.Marshal(status)
	if err != nil {
		t.Fatal(err)
	}
	var wire map[string]any
	if err := json.Unmarshal(payload, &wire); err != nil {
		t.Fatal(err)
	}
	for _, field := range []string{"endpoints", "links"} {
		items, ok := wire[field].([]any)
		if !ok {
			t.Fatalf("%s must be a JSON array, got %s", field, payload)
		}
		if len(items) != 0 {
			t.Fatalf("%s must be empty, got %s", field, payload)
		}
	}
	digest, ok := wire["observed_projection_sha256"].(string)
	if !ok || !regexp.MustCompile(`^[0-9a-f]{64}$`).MatchString(digest) {
		t.Fatalf("present projection must report a lowercase SHA-256 digest, got %s", payload)
	}
}

func TestEffectiveProjectionDigestIsCanonicalAndCoversRoutesAndGrants(t *testing.T) {
	request := validRequest("gateway")
	secondRoute := request.Routes[0]
	secondRoute.BindingID = "binding-0"
	secondRoute.RequirementName = "judge_control"
	secondRoute.ConsumerDeploymentID = "consumer-z"
	secondRoute.APIID = "judge.worker.control"
	secondRoute.Permission = "judge.worker"
	secondRoute.Methods = []string{"POST", "GET"}
	secondGrant := request.Grants[0]
	secondGrant.BindingID = secondRoute.BindingID
	secondGrant.RequirementName = secondRoute.RequirementName
	secondGrant.ConsumerDeploymentID = secondRoute.ConsumerDeploymentID
	secondGrant.APIID = secondRoute.APIID
	secondGrant.Permission = secondRoute.Permission

	routes := []BindingRoute{request.Routes[0], secondRoute}
	grants := []BindingGrant{request.Grants[0], secondGrant}
	canonical, err := CanonicalEffectiveProjectionJSON(routes, grants)
	if err != nil {
		t.Fatal(err)
	}
	reordered, err := CanonicalEffectiveProjectionJSON(
		[]BindingRoute{secondRoute, request.Routes[0]},
		[]BindingGrant{secondGrant, request.Grants[0]},
	)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(canonical, reordered) {
		t.Fatalf("input ordering changed canonical projection:\n%s\n%s", canonical, reordered)
	}
	if bytes.Index(canonical, []byte(`"binding_id":"binding-0"`)) > bytes.Index(canonical, []byte(`"binding_id":"binding-1"`)) {
		t.Fatalf("bindings were not ordered by binding_id: %s", canonical)
	}
	if !bytes.HasPrefix(canonical, []byte(`{"routes":[{"binding_id":`)) ||
		!bytes.Contains(canonical, []byte(`}],"grants":[{"binding_id":`)) {
		t.Fatalf("canonical field order changed: %s", canonical)
	}

	digest, err := EffectiveProjectionSHA256(routes, grants)
	if err != nil {
		t.Fatal(err)
	}
	reorderedDigest, err := EffectiveProjectionSHA256(
		[]BindingRoute{secondRoute, request.Routes[0]},
		[]BindingGrant{secondGrant, request.Grants[0]},
	)
	if err != nil {
		t.Fatal(err)
	}
	if digest != reorderedDigest {
		t.Fatalf("input ordering changed projection digest: %s != %s", digest, reorderedDigest)
	}
	changedRoutes := append([]BindingRoute(nil), routes...)
	changedRoutes[0].TimeoutMS++
	changedRouteDigest, err := EffectiveProjectionSHA256(changedRoutes, grants)
	if err != nil {
		t.Fatal(err)
	}
	if changedRouteDigest == digest {
		t.Fatal("route mutation did not change projection digest")
	}
	changedGrants := append([]BindingGrant(nil), grants...)
	changedGrants[0].CredentialGeneration++
	changedGrantDigest, err := EffectiveProjectionSHA256(routes, changedGrants)
	if err != nil {
		t.Fatal(err)
	}
	if changedGrantDigest == digest {
		t.Fatal("grant mutation did not change projection digest")
	}
}

func TestEffectiveProjectionEmptyWireFormIsStable(t *testing.T) {
	payload, err := CanonicalEffectiveProjectionJSON(nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	if string(payload) != `{"routes":[],"grants":[]}` {
		t.Fatalf("unexpected empty canonical projection: %s", payload)
	}
	digest, err := EffectiveProjectionSHA256(nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	if digest != "fa9d28278a0d02b19bfebeae5afd5aa6dde1c685d8396acc8defe8832848865c" {
		t.Fatalf("empty projection digest changed: %s", digest)
	}
}

func TestPresentStatusReportsProjectionDigestAndAbsentStatusOmitsIt(t *testing.T) {
	document := validRequest("auth").Document()
	status, err := document.Status()
	if err != nil {
		t.Fatal(err)
	}
	expected, err := EffectiveProjectionSHA256(document.Routes, document.Grants)
	if err != nil {
		t.Fatal(err)
	}
	if expected != "afcaf1f6a8b8be8ae64fa9f7e14d645e3a66657fdeac42cfe8db349b2ba0efbd" {
		t.Fatalf("non-empty projection digest changed: %s", expected)
	}
	if status.ObservedProjectionSHA256 == nil || *status.ObservedProjectionSHA256 != expected {
		t.Fatalf("present status projection digest = %v, want %s", status.ObservedProjectionSHA256, expected)
	}
	presentJSON, err := json.Marshal(status)
	if err != nil {
		t.Fatal(err)
	}
	var presentWire map[string]any
	if err := json.Unmarshal(presentJSON, &presentWire); err != nil {
		t.Fatal(err)
	}
	if presentWire["observed_projection_sha256"] != expected {
		t.Fatalf("present status omitted projection digest: %s", presentJSON)
	}

	absent := AbsentStatus("auth", "primary")
	if absent.ObservedProjectionSHA256 != nil {
		t.Fatal("absent status must not carry a projection digest")
	}
	absentJSON, err := json.Marshal(absent)
	if err != nil {
		t.Fatal(err)
	}
	var absentWire map[string]any
	if err := json.Unmarshal(absentJSON, &absentWire); err != nil {
		t.Fatal(err)
	}
	if _, exists := absentWire["observed_projection_sha256"]; exists {
		t.Fatalf("absent status must omit projection digest: %s", absentJSON)
	}
}

func TestDeleteAcceptsNullableSpecFromOrchestrator(t *testing.T) {
	// Rust's Option::None serializes as JSON null unless the field is explicitly
	// skipped.  A null spec and null desired revision/hash both describe the
	// absent projection required by a first-revision compensation.
	for name, payload := range map[string]string{
		"nullable spec": `{"api_version":"v1","provider":"gateway","action":"delete","topology_id":"primary","attempted_revision_id":"primary:r1:abc","desired_revision_id":null,"desired_content_sha256":null,"operation_id":"operation-1","spec":null,"routes":[],"grants":[]}`,
		"omitted spec":  `{"api_version":"v1","provider":"gateway","action":"delete","topology_id":"primary","attempted_revision_id":"primary:r1:abc","desired_revision_id":null,"desired_content_sha256":null,"operation_id":"operation-1","routes":[],"grants":[]}`,
	} {
		t.Run(name, func(t *testing.T) {
			request, err := DecodeRequest([]byte(payload))
			if err != nil {
				t.Fatalf("decode Orchestrator delete request: %v", err)
			}
			if err := request.Validate("gateway", "primary"); err != nil {
				t.Fatalf("valid absent projection was rejected: %v", err)
			}
		})
	}
}

func TestDeleteStillRejectsConcreteSpec(t *testing.T) {
	request := validRequest("auth")
	request.Action = "delete"
	request.DesiredRevisionID = nil
	request.DesiredContentSHA256 = nil
	request.Routes = nil
	request.Grants = nil
	if err := request.Validate("auth", "primary"); err == nil {
		t.Fatal("delete with a concrete topology spec was accepted")
	}
}

func TestPlanApplyAllowsOnlyExactSameOperationReplay(t *testing.T) {
	request := validRequest("gateway")
	current := request.Document()
	// PostgreSQL JSONB may reorder object keys when it persists the document.
	// Replay identity is structural JSON identity, not byte formatting.
	current.Spec = json.RawMessage(`{"links": [], "topology_id": "primary", "endpoints": []}`)

	write, err := PlanApply(&current, request)
	if err != nil || write {
		t.Fatalf("exact replay was not recognized: write=%v err=%v", write, err)
	}

	changed := validRequest("gateway")
	changed.Routes[0].TimeoutMS++
	if write, err := PlanApply(&current, changed); err == nil || write {
		t.Fatalf("same operation with changed projection was accepted: write=%v err=%v", write, err)
	}
}

func TestPlanApplyRestorePreviousCASAndReplay(t *testing.T) {
	attempt := validRequest("auth")
	current := attempt.Document()
	previousRevision := "revision-1"
	restore := validRequest("auth")
	restore.Action = "restore_previous"
	restore.DesiredRevisionID = &previousRevision

	if err := restore.Validate("auth", "primary"); err != nil {
		t.Fatalf("valid restore rejected: %v", err)
	}
	write, err := PlanApply(&current, restore)
	if err != nil || !write {
		t.Fatalf("attempted revision was not restored: write=%v err=%v", write, err)
	}

	restored := restore.Document()
	write, err = PlanApply(&restored, restore)
	if err != nil || write {
		t.Fatalf("restore replay was not idempotent: write=%v err=%v", write, err)
	}

	tampered := restore
	tampered.Routes = append([]BindingRoute(nil), restore.Routes...)
	tampered.Routes[0].TimeoutMS++
	if write, err := PlanApply(&restored, tampered); err == nil || write {
		t.Fatalf("restore replay with a different desired projection was accepted: write=%v err=%v", write, err)
	}
}

func TestPlanApplyRestorePreviousFailsClosedOnStaleOrForeignState(t *testing.T) {
	previousRevision := "revision-1"
	restore := validRequest("gateway")
	restore.Action = "restore_previous"
	restore.DesiredRevisionID = &previousRevision

	if write, err := PlanApply(nil, restore); err == nil || write {
		t.Fatalf("restore against an absent projection was accepted: write=%v err=%v", write, err)
	}

	foreignOperation := validRequest("gateway").Document()
	foreignOperation.OperationID = "operation-other"
	if write, err := PlanApply(&foreignOperation, restore); err == nil || write {
		t.Fatalf("restore from another operation was accepted: write=%v err=%v", write, err)
	}

	staleRevision := validRequest("gateway").Document()
	staleRevision.RevisionID = "revision-3"
	if write, err := PlanApply(&staleRevision, restore); err == nil || write {
		t.Fatalf("restore over a newer revision was accepted: write=%v err=%v", write, err)
	}
}

func TestProjectionActionRevisionRelationshipIsValidated(t *testing.T) {
	request := validRequest("gateway")
	wrongDesired := "revision-other"
	request.DesiredRevisionID = &wrongDesired
	if err := request.Validate("gateway", "primary"); err == nil {
		t.Fatal("apply with a desired revision different from attempted was accepted")
	}

	restore := validRequest("gateway")
	restore.Action = "restore_previous"
	if err := restore.Validate("gateway", "primary"); err == nil {
		t.Fatal("restore to the attempted revision was accepted")
	}
}
