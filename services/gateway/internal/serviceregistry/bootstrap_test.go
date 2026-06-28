package serviceregistry

import (
	"context"
	"testing"
)

type recordingWriter struct {
	sets           map[string]int
	services       map[string]int
	edges          map[string]int
	components     map[string]int
	installations  map[string]int
	permissions    map[string]int
	menus          map[string]int
	frontendRoutes map[string]int
	gatewayRoutes  map[string]int
	migrations     map[string]int
}

func newRecordingWriter() *recordingWriter {
	return &recordingWriter{
		sets:           map[string]int{},
		services:       map[string]int{},
		edges:          map[string]int{},
		components:     map[string]int{},
		installations:  map[string]int{},
		permissions:    map[string]int{},
		menus:          map[string]int{},
		frontendRoutes: map[string]int{},
		gatewayRoutes:  map[string]int{},
		migrations:     map[string]int{},
	}
}

func (w *recordingWriter) UpsertSet(_ context.Context, item Set) error {
	w.sets[item.SetID]++
	return nil
}

func (w *recordingWriter) UpsertService(_ context.Context, item Service) error {
	w.services[item.ServiceID]++
	return nil
}

func (w *recordingWriter) UpsertEdge(_ context.Context, item Edge) error {
	w.edges[item.FromServiceID+"->"+item.ToServiceID+":"+item.EdgeType]++
	return nil
}

func (w *recordingWriter) UpsertComponent(_ context.Context, item Component) error {
	w.components[item.ServiceID+"/"+item.ComponentID]++
	return nil
}

func (w *recordingWriter) UpsertInstallation(_ context.Context, item Installation) error {
	w.installations[item.ServiceID]++
	return nil
}

func (w *recordingWriter) UpsertPermission(_ context.Context, item Permission) error {
	w.permissions[item.PermissionKey]++
	return nil
}

func (w *recordingWriter) UpsertMenu(_ context.Context, item Menu) error {
	w.menus[item.MenuKey]++
	return nil
}

func (w *recordingWriter) UpsertFrontendRoute(_ context.Context, item FrontendRoute) error {
	w.frontendRoutes[item.ServiceID+item.RoutePath]++
	return nil
}

func (w *recordingWriter) UpsertGatewayRoute(_ context.Context, item GatewayRoute) error {
	w.gatewayRoutes[item.Prefix]++
	return nil
}

func (w *recordingWriter) UpsertMigration(_ context.Context, item Migration) error {
	w.migrations[item.ServiceID+item.MigrationName]++
	return nil
}

func TestBuiltinDataContainsBaseServicesTopology(t *testing.T) {
	data := BuiltinData()

	if len(data.Sets) == 0 || len(data.Services) == 0 || len(data.Edges) == 0 || len(data.Components) == 0 {
		t.Fatalf("expected non-empty sets/services/edges/components")
	}

	for _, serviceID := range []string{
		"root-runtime-manager",
		"gateway",
		"web-shell",
		"problem-api",
		"judge-api",
		"judge-worker",
		"storage",
		"postgres",
	} {
		if !bootstrapHasService(data.Services, serviceID) {
			t.Fatalf("builtin service %s not found", serviceID)
		}
	}
	if !bootstrapHasEdge(data.Edges, "judge-worker", "judge-api") {
		t.Fatalf("judge-worker should link to judge-api")
	}
	if !bootstrapHasEdge(data.Edges, "gateway", "problem-api") {
		t.Fatalf("gateway should route to problem-api")
	}
}

func TestBootstrapBuiltinIsRepeatable(t *testing.T) {
	writer := newRecordingWriter()
	ctx := context.Background()

	if err := BootstrapBuiltin(ctx, writer); err != nil {
		t.Fatalf("first bootstrap failed: %v", err)
	}
	if err := BootstrapBuiltin(ctx, writer); err != nil {
		t.Fatalf("second bootstrap failed: %v", err)
	}

	for key, count := range writer.services {
		if count != 2 {
			t.Fatalf("expected service %s to be upserted twice, got %d", key, count)
		}
	}
	if writer.services["judge-worker"] != 2 {
		t.Fatalf("judge-worker should be bootstrapped twice")
	}
	if writer.permissions["judge.submit"] != 2 {
		t.Fatalf("judge.submit permission should be bootstrapped twice")
	}
	if writer.permissions["system.admin"] != 2 {
		t.Fatalf("system.admin permission should be bootstrapped twice")
	}
}

func bootstrapHasService(items []Service, serviceID string) bool {
	for _, item := range items {
		if item.ServiceID == serviceID {
			return true
		}
	}
	return false
}

func bootstrapHasEdge(items []Edge, from string, to string) bool {
	for _, item := range items {
		if item.FromServiceID == from && item.ToServiceID == to {
			return true
		}
	}
	return false
}
