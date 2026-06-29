package handler

import (
	"os"
	"regexp"
	"strings"
	"testing"
)

func TestAdminServiceTopologyRoutePrecedesDetailRoute(t *testing.T) {
	data, err := os.ReadFile("routes.go")
	if err != nil {
		t.Fatal(err)
	}
	source := string(data)

	sets := strings.Index(source, `"/api/admin/sets"`)
	topology := strings.Index(source, `"/api/admin/topology"`)
	serviceSnapshot := strings.Index(source, `"/api/admin/orchestrator/snapshot"`)
	routeTableRoutes := strings.Index(source, `"/api/admin/orchestrator/routes"`)
	status := strings.Index(source, `"/api/admin/services/status"`)
	statusOperations := strings.Index(source, `"/api/admin/services/status/operations"`)
	statusDetail := strings.Index(source, `"/api/admin/services/status/:id"`)
	detail := strings.Index(source, `"/api/admin/services/:id"`)
	if sets < 0 {
		t.Fatalf("sets route not found")
	}
	if topology < 0 {
		t.Fatalf("topology route not found")
	}
	if detail < 0 {
		t.Fatalf("service detail route not found")
	}
	if serviceSnapshot < 0 {
		t.Fatalf("orchestrator snapshot route not found")
	}
	if routeTableRoutes < 0 {
		t.Fatalf("service route table routes not found")
	}
	if status < 0 {
		t.Fatalf("service status route not found")
	}
	if statusOperations < 0 {
		t.Fatalf("service status operations route not found")
	}
	if statusDetail < 0 {
		t.Fatalf("service status detail route not found")
	}
	if topology > detail {
		t.Fatalf("topology route must be registered before service detail route")
	}
	if serviceSnapshot > detail {
		t.Fatalf("orchestrator snapshot route must be registered before service detail route")
	}
	if routeTableRoutes > detail {
		t.Fatalf("service route table routes must be registered before service detail route")
	}
	if sets > detail {
		t.Fatalf("sets route must be registered before service detail route")
	}
	if status > detail {
		t.Fatalf("service status route must be registered before service detail route")
	}
	if statusOperations > statusDetail {
		t.Fatalf("service status operations route must be registered before service status detail route")
	}
}

func TestOrchestratorAdminRoutesAreReadOnly(t *testing.T) {
	data, err := os.ReadFile("routes.go")
	if err != nil {
		t.Fatal(err)
	}

	routes := parseRouteMethods(string(data))
	if len(routes) == 0 {
		t.Fatalf("no routes parsed from routes.go")
	}

	for _, route := range routes {
		if !isOrchestratorAdminRoute(route.path) {
			continue
		}
		if route.method != "MethodGet" && route.method != "MethodOptions" {
			t.Fatalf("orchestrator admin route %s must be read-only, got http.%s", route.path, route.method)
		}
	}
}

type routeMethod struct {
	method string
	path   string
}

func parseRouteMethods(source string) []routeMethod {
	pattern := regexp.MustCompile(`(?s)Method:\s+http\.(Method[A-Za-z]+),\s*Path:\s+"([^"]+)"`)
	matches := pattern.FindAllStringSubmatch(source, -1)
	routes := make([]routeMethod, 0, len(matches))
	for _, match := range matches {
		routes = append(routes, routeMethod{method: match[1], path: match[2]})
	}
	return routes
}

func isOrchestratorAdminRoute(path string) bool {
	return path == "/api/admin/services" ||
		strings.HasPrefix(path, "/api/admin/services/") ||
		path == "/api/admin/sets" ||
		strings.HasPrefix(path, "/api/admin/sets/") ||
		path == "/api/admin/topology" ||
		strings.HasPrefix(path, "/api/admin/topology/") ||
		path == "/api/admin/orchestrator" ||
		strings.HasPrefix(path, "/api/admin/orchestrator/")
}
