package topologyprojection

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"ojos-gateway/internal/orchestrator/servicestatus"
	"ojos-gateway/internal/proxy"
	"ojos-shared/security/workload"
	shared "ojos-shared/topologyprojection"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
	"go.uber.org/zap"
)

func gatewayRequest(topologyID, revisionID, operationID string) shared.Request {
	hash := "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	return shared.Request{
		APIVersion: shared.APIVersion, Provider: "gateway", Action: "apply",
		TopologyID: topologyID, AttemptedRevisionID: revisionID,
		DesiredRevisionID: &revisionID, DesiredContentSHA256: &hash, OperationID: operationID,
		Spec:   json.RawMessage(`{"topology_id":"` + topologyID + `","endpoints":[],"links":[]}`),
		Routes: []shared.BindingRoute{}, Grants: []shared.BindingGrant{},
	}
}

func gatewayRedisStore(t *testing.T, address string) *Store {
	t.Helper()
	client := redis.NewClient(&redis.Options{Addr: address})
	t.Cleanup(func() { _ = client.Close() })
	serviceProxy, err := proxy.NewServiceProxy(nil, nil, "", nil, zap.NewNop())
	if err != nil {
		t.Fatal(err)
	}
	return NewStore(client, serviceProxy)
}

func TestRouteTableScopesSameAPIByConsumerDeployment(t *testing.T) {
	route := func(binding, consumer, provider string) shared.BindingRoute {
		return shared.BindingRoute{
			BindingID: binding, RequirementName: "judge_control", ConsumerDeploymentID: consumer,
			ConsumerServiceID: "judge-worker", ConsumerNodeID: "node-b",
			CredentialGeneration: 4, APIID: "judge.worker.control",
			ProviderDeploymentID: provider, ProviderServiceID: "judge-api", ProviderNodeID: "node-a",
			ProviderEndpoint: "10.0.0.1:8080:judge-api", UpstreamBase: "https://10.0.0.1:8080",
			ProviderPath: "/worker", VirtualPath: "/internal/apis/judge.worker.control",
			AuthMode: "workload", ProviderAuthMode: "workload", Permission: "judge.worker", Methods: []string{"POST"}, TimeoutMS: 35000,
		}
	}
	routeA := route("binding-a", "worker-a", "judge-a")
	routeB := route("binding-b", "worker-b", "judge-b")
	grant := func(route shared.BindingRoute) shared.BindingGrant {
		return shared.BindingGrant{
			BindingID: route.BindingID, RequirementName: route.RequirementName,
			ConsumerDeploymentID: route.ConsumerDeploymentID, ConsumerServiceID: route.ConsumerServiceID,
			ConsumerNodeID: route.ConsumerNodeID, CredentialGeneration: route.CredentialGeneration,
			APIID: route.APIID, Permission: route.Permission,
		}
	}
	table := routeTable(map[string]shared.Document{
		"primary": {Routes: []shared.BindingRoute{routeA, routeB}, Grants: []shared.BindingGrant{grant(routeA), grant(routeB)}},
	})
	if len(table.Routes) != 2 {
		t.Fatalf("expected two consumer-scoped routes, got %d", len(table.Routes))
	}
	if table.Routes[0].ConsumerDeploymentID == table.Routes[1].ConsumerDeploymentID ||
		table.Routes[0].ApiID != table.Routes[1].ApiID {
		t.Fatal("same API was not retained as two consumer-scoped routes")
	}
}

func TestTopologyProjectionRoutesInternalAPIToProviderPath(t *testing.T) {
	var gotPaths []string
	var gotAuthorizations []string
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPaths = append(gotPaths, r.URL.Path)
		gotAuthorizations = append(gotAuthorizations, r.Header.Get("Authorization"))
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	issuer, err := workload.NewIssuer(
		privateKey,
		"workload-1",
		"issuer",
		"gateway",
		15*time.Minute,
	)
	if err != nil {
		t.Fatal(err)
	}
	verifier, err := workload.NewVerifier(publicKey, "workload-1", "issuer", "gateway")
	if err != nil {
		t.Fatal(err)
	}
	token, _, err := issuer.Issue(workload.IssueRequest{
		DeploymentID:         "deployment-problem-a",
		ServiceID:            "problem-service",
		NodeID:               "node-a",
		CredentialGeneration: 3,
	}, time.Now())
	if err != nil {
		t.Fatal(err)
	}

	server := miniredis.RunT(t)
	store := gatewayRedisStore(t, server.Addr())
	store.proxy.SetWorkloadVerifier(verifier)
	route := shared.BindingRoute{
		BindingID:            "binding-permission-check",
		RequirementName:      "permission_check",
		ConsumerDeploymentID: "deployment-problem-a",
		ConsumerServiceID:    "problem-service",
		ConsumerNodeID:       "node-a",
		CredentialGeneration: 3,
		APIID:                "auth.user.permission.check",
		ProviderDeploymentID: "deployment-auth-a",
		ProviderServiceID:    "auth-service",
		ProviderNodeID:       "external",
		ProviderEndpoint:     "127.0.0.1:8081:auth-service",
		UpstreamBase:         upstream.URL,
		ProviderPath:         "/auth/admin/permission-check",
		VirtualPath:          "/internal/apis/auth.user.permission.check",
		AuthMode:             "workload",
		ProviderAuthMode:     "workload",
		Permission:           "auth.permission.check",
		Methods:              []string{http.MethodPost},
		TimeoutMS:            5000,
	}
	grant := shared.BindingGrant{
		BindingID:            route.BindingID,
		RequirementName:      route.RequirementName,
		ConsumerDeploymentID: route.ConsumerDeploymentID,
		ConsumerServiceID:    route.ConsumerServiceID,
		ConsumerNodeID:       route.ConsumerNodeID,
		CredentialGeneration: route.CredentialGeneration,
		APIID:                route.APIID,
		Permission:           route.Permission,
	}
	storageRoute := route
	storageRoute.BindingID = "binding-storage-get"
	storageRoute.RequirementName = "storage_get"
	storageRoute.APIID = "storage.object.get"
	storageRoute.ProviderDeploymentID = "deployment-storage-a"
	storageRoute.ProviderServiceID = "storage-service"
	storageRoute.ProviderEndpoint = "127.0.0.1:8085:storage-service"
	storageRoute.ProviderPath = "/objects"
	storageRoute.VirtualPath = "/internal/apis/storage.object.get"
	storageRoute.Permission = "storage.object.read"
	storageRoute.Methods = []string{http.MethodGet}
	storageGrant := grant
	storageGrant.BindingID = storageRoute.BindingID
	storageGrant.RequirementName = storageRoute.RequirementName
	storageGrant.APIID = storageRoute.APIID
	storageGrant.Permission = storageRoute.Permission
	projection := gatewayRequest("primary", "revision-1", "operation-1")
	projection.Routes = []shared.BindingRoute{route, storageRoute}
	projection.Grants = []shared.BindingGrant{grant, storageGrant}
	if err := projection.Validate("gateway", "primary"); err != nil {
		t.Fatal(err)
	}
	if err := store.Apply(context.Background(), projection); err != nil {
		t.Fatal(err)
	}

	req := httptest.NewRequest(
		http.MethodPost,
		"/internal/apis/auth.user.permission.check",
		strings.NewReader(`{"user_id":42,"permission":"problem.create"}`),
	)
	req.Header.Set("Authorization", "Bearer "+token)
	rr := httptest.NewRecorder()
	store.proxy.ServeHTTP(rr, req)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("projected route returned %d: %s", rr.Code, rr.Body.String())
	}

	req = httptest.NewRequest(
		http.MethodGet,
		"/internal/apis/storage.object.get/buckets/problems/object.zip",
		nil,
	)
	req.Header.Set("Authorization", "Bearer "+token)
	rr = httptest.NewRecorder()
	store.proxy.ServeHTTP(rr, req)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("projected tail route returned %d: %s", rr.Code, rr.Body.String())
	}
	if len(gotPaths) != 2 || gotPaths[0] != "/auth/admin/permission-check" ||
		gotPaths[1] != "/objects/buckets/problems/object.zip" {
		t.Fatalf("projected provider paths were not applied: got %#v", gotPaths)
	}
	if len(gotAuthorizations) != 2 || gotAuthorizations[0] != "Bearer "+token ||
		gotAuthorizations[1] != "Bearer "+token {
		t.Fatalf("workload credentials were not forwarded: %#v", gotAuthorizations)
	}
}

func TestRedisProjectionRecoverRestoresWorkloadRouteBeforeAndAfterBaseReload(t *testing.T) {
	var gotPaths []string
	var gotAuthorizations []string
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPaths = append(gotPaths, r.URL.Path)
		gotAuthorizations = append(gotAuthorizations, r.Header.Get("Authorization"))
		w.WriteHeader(http.StatusNoContent)
	}))
	defer upstream.Close()

	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	issuer, err := workload.NewIssuer(privateKey, "workload-restart", "issuer", "gateway", 15*time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	verifier, err := workload.NewVerifier(publicKey, "workload-restart", "issuer", "gateway")
	if err != nil {
		t.Fatal(err)
	}
	token, _, err := issuer.Issue(workload.IssueRequest{
		DeploymentID:         "deployment-worker-b",
		ServiceID:            "judge-worker",
		NodeID:               "node-b",
		CredentialGeneration: 7,
	}, time.Now())
	if err != nil {
		t.Fatal(err)
	}

	route := shared.BindingRoute{
		BindingID:            "binding-judge-control",
		RequirementName:      "judge_control",
		ConsumerDeploymentID: "deployment-worker-b",
		ConsumerServiceID:    "judge-worker",
		ConsumerNodeID:       "node-b",
		CredentialGeneration: 7,
		APIID:                "judge.worker.control",
		ProviderDeploymentID: "deployment-judge-api-a",
		ProviderServiceID:    "judge-api",
		ProviderNodeID:       "node-a",
		ProviderEndpoint:     "127.0.0.1:8082:judge-api",
		UpstreamBase:         upstream.URL,
		ProviderPath:         "/api/judge/worker",
		VirtualPath:          "/internal/apis/judge.worker.control",
		AuthMode:             "workload",
		ProviderAuthMode:     "workload",
		Permission:           "judge.worker",
		Methods:              []string{http.MethodPost},
		TimeoutMS:            35000,
	}
	grant := shared.BindingGrant{
		BindingID:            route.BindingID,
		RequirementName:      route.RequirementName,
		ConsumerDeploymentID: route.ConsumerDeploymentID,
		ConsumerServiceID:    route.ConsumerServiceID,
		ConsumerNodeID:       route.ConsumerNodeID,
		CredentialGeneration: route.CredentialGeneration,
		APIID:                route.APIID,
		Permission:           route.Permission,
	}
	projection := gatewayRequest("restart-topology", "revision-1", "operation-1")
	projection.Routes = []shared.BindingRoute{route}
	projection.Grants = []shared.BindingGrant{grant}
	if err := projection.Validate("gateway", "restart-topology"); err != nil {
		t.Fatal(err)
	}

	server := miniredis.RunT(t)
	ctx := context.Background()
	storeA := gatewayRedisStore(t, server.Addr())
	if err := storeA.Apply(ctx, projection); err != nil {
		t.Fatal(err)
	}
	storeA.proxy.Close()
	if err := storeA.redis.Close(); err != nil {
		t.Fatalf("close first Gateway Redis client: %v", err)
	}

	// Store B owns a completely new ServiceProxy and Redis client, just as a
	// restarted Gateway process would. Its only topology source is Redis.
	storeB := gatewayRedisStore(t, server.Addr())
	storeB.proxy.SetWorkloadVerifier(verifier)
	if err := storeB.Recover(ctx); err != nil {
		t.Fatalf("recover topology projection from Redis: %v", err)
	}

	assertWorkloadRoute := func(stage string) {
		t.Helper()
		req := httptest.NewRequest(
			http.MethodPost,
			"/internal/apis/judge.worker.control/register",
			strings.NewReader(`{"worker_id":"deployment-worker-b"}`),
		)
		req.Header.Set("Authorization", "Bearer "+token)
		rr := httptest.NewRecorder()
		storeB.proxy.ServeHTTP(rr, req)
		if rr.Code != http.StatusNoContent {
			t.Fatalf("%s: recovered workload route returned %d: %s", stage, rr.Code, rr.Body.String())
		}
	}

	assertWorkloadRoute("after Recover")
	storeB.proxy.SetRouteTable(servicestatus.RouteTable{
		Version:     "node-route-snapshot-1",
		GeneratedAt: time.Now().UTC().Format(time.RFC3339Nano),
		CanProxy:    true,
	})
	assertWorkloadRoute("after base route snapshot reload")

	if len(gotPaths) != 2 || gotPaths[0] != "/api/judge/worker/register" || gotPaths[1] != "/api/judge/worker/register" {
		t.Fatalf("recovered route did not preserve provider path across base reload: %#v", gotPaths)
	}
	if len(gotAuthorizations) != 2 || gotAuthorizations[0] != "Bearer "+token || gotAuthorizations[1] != "Bearer "+token {
		t.Fatalf("recovered workload credentials were not forwarded across base reload: %#v", gotAuthorizations)
	}
}

func TestDuplicateConsumerRequirementFailsClosed(t *testing.T) {
	route := shared.BindingRoute{BindingID: "binding-a", RequirementName: "storage_get", ConsumerDeploymentID: "worker-b", ConsumerServiceID: "judge-worker", ConsumerNodeID: "node-b", CredentialGeneration: 1}
	duplicate := route
	duplicate.BindingID = "binding-b"
	table := routeTable(map[string]shared.Document{
		"a": {Routes: []shared.BindingRoute{route}, Grants: []shared.BindingGrant{{BindingID: route.BindingID, RequirementName: route.RequirementName, ConsumerDeploymentID: route.ConsumerDeploymentID, ConsumerServiceID: route.ConsumerServiceID, ConsumerNodeID: route.ConsumerNodeID, CredentialGeneration: route.CredentialGeneration}}},
		"b": {Routes: []shared.BindingRoute{duplicate}, Grants: []shared.BindingGrant{{BindingID: duplicate.BindingID, RequirementName: duplicate.RequirementName, ConsumerDeploymentID: duplicate.ConsumerDeploymentID, ConsumerServiceID: duplicate.ConsumerServiceID, ConsumerNodeID: duplicate.ConsumerNodeID, CredentialGeneration: duplicate.CredentialGeneration}}},
	})
	if len(table.Routes) != 1 || len(table.Warnings) != 1 {
		t.Fatalf("duplicate route was not suppressed: routes=%d warnings=%v", len(table.Routes), table.Warnings)
	}
}

func TestRouteTablePreservesProviderAuthenticationMode(t *testing.T) {
	route := shared.BindingRoute{
		BindingID: "binding-public", RequirementName: "public_get", ConsumerDeploymentID: "worker-b",
		ConsumerServiceID: "judge-worker", ConsumerNodeID: "node-b",
		CredentialGeneration: 2, APIID: "asset.public.get", ProviderDeploymentID: "asset-a",
		ProviderServiceID: "asset", ProviderNodeID: "node-a", ProviderEndpoint: "10.0.0.1:8080:asset",
		UpstreamBase: "https://10.0.0.1:8080", ProviderPath: "/objects", VirtualPath: "/internal/apis/asset.public.get",
		AuthMode: "workload", ProviderAuthMode: "public", Permission: "public", Methods: []string{"GET"}, TimeoutMS: 5000,
	}
	grant := shared.BindingGrant{BindingID: route.BindingID, RequirementName: route.RequirementName, ConsumerDeploymentID: route.ConsumerDeploymentID, ConsumerServiceID: route.ConsumerServiceID, ConsumerNodeID: route.ConsumerNodeID, CredentialGeneration: route.CredentialGeneration, APIID: route.APIID, Permission: route.Permission}
	table := routeTable(map[string]shared.Document{"main": {Routes: []shared.BindingRoute{route}, Grants: []shared.BindingGrant{grant}}})
	if len(table.Routes) != 1 || table.Routes[0].AuthMode != "workload" || table.Routes[0].ProviderAuthMode != "public" {
		t.Fatalf("provider auth mode was lost: %#v", table.Routes)
	}
}

func TestRedisProjectionRestorePreviousIsCASAndIdempotent(t *testing.T) {
	server := miniredis.RunT(t)
	store := gatewayRedisStore(t, server.Addr())
	ctx := context.Background()

	attempt := gatewayRequest("primary", "revision-2", "operation-2")
	if err := store.Apply(ctx, attempt); err != nil {
		t.Fatal(err)
	}
	previousRevision := "revision-1"
	restore := gatewayRequest("primary", "revision-2", "operation-2")
	restore.Action = "restore_previous"
	restore.DesiredRevisionID = &previousRevision
	if err := restore.Validate("gateway", "primary"); err != nil {
		t.Fatal(err)
	}
	if err := store.Apply(ctx, restore); err != nil {
		t.Fatalf("restore failed: %v", err)
	}
	if err := store.Apply(ctx, restore); err != nil {
		t.Fatalf("restore replay failed: %v", err)
	}
	document, err := store.Get(ctx, "primary")
	if err != nil || document == nil || document.RevisionID != previousRevision || document.OperationID != "operation-2" {
		t.Fatalf("unexpected restored document: document=%v err=%v", document, err)
	}

	wrongOperation := restore
	wrongOperation.OperationID = "operation-other"
	if err := store.Apply(ctx, wrongOperation); err == nil {
		t.Fatal("restore from another operation was accepted")
	}
	document, err = store.Get(ctx, "primary")
	if err != nil || document == nil || document.RevisionID != previousRevision {
		t.Fatalf("rejected restore changed Redis state: document=%v err=%v", document, err)
	}
}

func TestRedisProjectionConcurrentNewApplyWinsOverStaleRestore(t *testing.T) {
	server := miniredis.RunT(t)
	storeA := gatewayRedisStore(t, server.Addr())
	storeB := gatewayRedisStore(t, server.Addr())
	ctx := context.Background()

	for index := 0; index < 12; index++ {
		topologyID := "race-" + string(rune('a'+index))
		attempt := gatewayRequest(topologyID, "revision-2", "operation-2")
		if err := storeA.Apply(ctx, attempt); err != nil {
			t.Fatal(err)
		}
		previousRevision := "revision-1"
		restore := gatewayRequest(topologyID, "revision-2", "operation-2")
		restore.Action = "restore_previous"
		restore.DesiredRevisionID = &previousRevision
		newer := gatewayRequest(topologyID, "revision-3", "operation-3")

		var wait sync.WaitGroup
		wait.Add(2)
		var applyErr error
		go func() {
			defer wait.Done()
			_ = storeA.Apply(ctx, restore) // Either wins first or fails closed after revision-3.
		}()
		go func() {
			defer wait.Done()
			applyErr = storeB.Apply(ctx, newer)
		}()
		wait.Wait()
		if applyErr != nil {
			t.Fatalf("newer apply failed: %v", applyErr)
		}
		document, err := storeA.Get(ctx, topologyID)
		if err != nil || document == nil || document.RevisionID != "revision-3" || document.OperationID != "operation-3" {
			t.Fatalf("stale restore overwrote newer apply: document=%v err=%v", document, err)
		}
	}
}
