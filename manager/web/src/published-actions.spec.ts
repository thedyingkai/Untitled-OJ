import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { parse } from "yaml";
import { describe, expect, it } from "vitest";
import { api } from "./api";
import {
  WEB_V1_ACTION_METHODS,
  WEB_V1_ACTIONS,
  WEB_V1_ROLE_PERMISSIONS,
} from "./published-actions";

interface PublishedMatrix {
  schema_version: number;
  api_version: string;
  roles: Record<string, string>;
  actions: Array<{
    action: string;
    target_type: string;
    role: string;
    asynchronous: boolean;
  }>;
}

describe("published v1 action fixture", () => {
  it("publishes the complete Store validation pipeline and typed Topology diff", () => {
    const path = resolve(
      process.cwd(),
      "../../platform/schemas/orchestrator/openapi-v1.yaml",
    );
    const contract = parse(readFileSync(path, "utf8")) as any;
    const selection = contract.components.schemas.StoreValidateRequest;
    const install = contract.components.schemas.StoreInstallRequest;
    const validation = contract.components.schemas.StoreValidationResult;

    for (const field of [
      "start",
      "migration_policy",
      "gateway_node_id",
      "config",
      "secret_refs",
    ]) {
      expect(selection.properties).toHaveProperty(field);
      expect(install.properties).toHaveProperty(field);
    }
    expect(
      contract.paths["/store/releases:validate"].post.responses["200"].$ref,
    ).toBe("#/components/responses/StoreValidation");
    expect(validation.required).toContain("topology_diff");
    expect(validation.properties.topology_diff.oneOf).toContainEqual({
      $ref: "#/components/schemas/TopologyDiff",
    });
  });

  it("matches the checked-in action/RBAC matrix byte-for-byte by field", () => {
    const path = resolve(
      process.cwd(),
      "../../platform/schemas/orchestrator/actions-v1.yaml",
    );
    const matrix = parse(readFileSync(path, "utf8")) as PublishedMatrix;

    expect(matrix.schema_version).toBe(1);
    expect(matrix.api_version).toBe("v1");
    expect(matrix.roles).toEqual(WEB_V1_ROLE_PERMISSIONS);
    expect(
      matrix.actions.map((item) => [
        item.action,
        item.target_type,
        item.role,
        item.asynchronous,
      ]),
    ).toEqual(WEB_V1_ACTIONS);
    expect(new Set(matrix.actions.map((item) => item.action)).size).toBe(
      WEB_V1_ACTIONS.length,
    );
  });

  it("has a concrete Web control method for every published capability", () => {
    const published = WEB_V1_ACTIONS.map(([action]) => action).sort();
    expect(Object.keys(WEB_V1_ACTION_METHODS).sort()).toEqual(published);
    for (const [action, method] of Object.entries(WEB_V1_ACTION_METHODS)) {
      expect(
        typeof api[method as keyof typeof api],
        `${action} -> api.${method}`,
      ).toBe("function");
    }
  });

  it("exposes the formerly SDK-only controls through reachable Web views", () => {
    const views = {
      store: readFileSync(resolve(process.cwd(), "src/views/StoreView.vue"), "utf8"),
      operations: readFileSync(
        resolve(process.cwd(), "src/views/OperationsView.vue"),
        "utf8",
      ),
      diagnostics: readFileSync(
        resolve(process.cwd(), "src/views/DiagnosticsView.vue"),
        "utf8",
      ),
    };
    for (const action of [
      "catalog.list",
      "catalog.search",
      "catalog.register",
      "catalog.remove",
    ]) {
      expect(views.store).toContain(`data-action="${action}"`);
    }
    expect(views.operations).toContain('data-action="operation.plan"');
    for (const action of [
      "diagnostic.create",
      "diagnostic.list",
      "diagnostic.get",
      "diagnostic.export",
    ]) {
      expect(views.diagnostics).toContain(`data-action="${action}"`);
    }

    const router = readFileSync(resolve(process.cwd(), "src/main.ts"), "utf8");
    const shell = readFileSync(resolve(process.cwd(), "src/App.vue"), "utf8");
    expect(router).toContain('path: "/diagnostics"');
    expect(shell).toContain('to: "/diagnostics"');
  });
});
