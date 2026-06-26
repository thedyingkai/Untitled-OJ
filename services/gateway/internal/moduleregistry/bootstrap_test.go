package moduleregistry

import (
	"context"
	"testing"
)

type recordingWriter struct {
	sets           map[string]int
	modules        map[string]int
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
		modules:        map[string]int{},
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

func (w *recordingWriter) UpsertModule(_ context.Context, item Module) error {
	w.modules[item.ModuleID]++
	return nil
}

func (w *recordingWriter) UpsertEdge(_ context.Context, item Edge) error {
	w.edges[item.FromModuleID+"->"+item.ToModuleID+":"+item.EdgeType]++
	return nil
}

func (w *recordingWriter) UpsertComponent(_ context.Context, item Component) error {
	w.components[item.ModuleID+"/"+item.ComponentID]++
	return nil
}

func (w *recordingWriter) UpsertInstallation(_ context.Context, item Installation) error {
	w.installations[item.ModuleID]++
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
	w.frontendRoutes[item.ModuleID+item.RoutePath]++
	return nil
}

func (w *recordingWriter) UpsertGatewayRoute(_ context.Context, item GatewayRoute) error {
	w.gatewayRoutes[item.Prefix]++
	return nil
}

func (w *recordingWriter) UpsertMigration(_ context.Context, item Migration) error {
	w.migrations[item.ModuleID+item.MigrationName]++
	return nil
}

func TestBuiltinDataContainsJudgeCoreTopology(t *testing.T) {
	data := BuiltinData()

	if len(data.Sets) == 0 || len(data.Modules) == 0 || len(data.Edges) == 0 || len(data.Components) == 0 {
		t.Fatalf("expected non-empty sets/modules/edges/components")
	}

	var foundJudgeCore bool
	for _, module := range data.Modules {
		if module.ModuleID == "ojos.judge-core" {
			foundJudgeCore = true
			if module.SetID != "core-capability" || module.Status != StatusEnabled || module.Kind != KindFeature {
				t.Fatalf("unexpected judge-core metadata: %#v", module)
			}
			if len(module.Manifest) == 0 {
				t.Fatalf("judge-core manifest should be embedded")
			}
		}
	}
	if !foundJudgeCore {
		t.Fatalf("judge-core module not found")
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

	for key, count := range writer.modules {
		if count != 2 {
			t.Fatalf("expected module %s to be upserted twice, got %d", key, count)
		}
	}
	if writer.modules["ojos.judge-core"] != 2 {
		t.Fatalf("judge-core should be bootstrapped twice")
	}
	if writer.permissions["problem.manage.data"] != 2 {
		t.Fatalf("problem.manage.data permission should be bootstrapped twice")
	}
	if writer.permissions["system.admin"] != 2 {
		t.Fatalf("system.admin permission should be bootstrapped twice")
	}
}
