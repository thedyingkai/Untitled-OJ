import { describe, expect, it } from "vitest";
import {
  activeConfigFields,
  compositionFormErrors,
  initializeCompositionState,
  serializeCompositionInputs,
} from "./composition-form";
import type { CompositionPlan } from "./types";

const conditionalSchema = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  type: "object",
  properties: {
    registration: {
      type: "object",
      properties: {
        mode: { type: "string", enum: ["open", "invite-only"] },
        inviteSigningKey: {
          type: "string",
          writeOnly: true,
          "x-ojos-secret": true,
        },
        sender: { type: "string" },
      },
      required: ["mode"],
      dependentRequired: { sender: ["mode"] },
      if: {
        properties: { mode: { const: "invite-only" } },
        required: ["mode"],
      },
      then: { required: ["inviteSigningKey"] },
      else: { not: { required: ["inviteSigningKey"] } },
    },
  },
  required: ["registration"],
};

function plan(): CompositionPlan {
  return {
    schemaVersion: "ojos.dev/composition-plan/v1",
    mode: "production",
    rootServiceId: "contest-service",
    planDigest: `sha256:${"1".repeat(64)}`,
    releaseGraphDigest: `sha256:${"2".repeat(64)}`,
    edges: [],
    nodes: [
      {
        nodeId: "config-node",
        serviceId: "contest-service",
        kind: "config",
        required: true,
        schema: conditionalSchema,
        unresolvedInputs: [
          {
            key: "config",
            valueType: "json-object",
            required: true,
            sensitive: false,
            allowedValues: [],
          },
        ],
      },
      {
        nodeId: "secret-node",
        serviceId: "contest-service",
        kind: "secret",
        name: "registration.inviteSigningKey",
        required: false,
        unresolvedInputs: [
          {
            key: "secretRef",
            valueType: "secret-ref",
            required: false,
            sensitive: true,
            allowedValues: [],
          },
        ],
      },
      {
        nodeId: "resource-node",
        serviceId: "contest-service",
        kind: "resource-claim",
        name: "contests",
        resourceType: "postgresql.database/v1",
        lifecycle: "RETAIN",
        provider: {
          capability: "postgresql.database",
          versionRequirement: "^1.0.0",
          policy: "unique-healthy",
          candidates: [
            {
              providerId: "postgres-local",
              version: "1.0.0",
              kind: "MANAGED",
            },
            {
              providerId: "postgres-external",
              version: "1.0.0",
              kind: "EXTERNAL",
            },
          ],
        },
        unresolvedInputs: [
          {
            key: "providerId",
            valueType: "provider-id",
            required: true,
            sensitive: false,
            allowedValues: ["postgres-external", "postgres-local"],
          },
        ],
      },
    ],
  };
}

describe("Composition dynamic form", () => {
  it("requires only the active conditional secret branch and never puts it in config", () => {
    const composition = plan();
    const state = initializeCompositionState(composition);
    state["contest-service"]["config-node"]["registration.mode"] = "open";
    state["contest-service"]["resource-node"].providerId = "postgres-local";

    expect(
      activeConfigFields(
        conditionalSchema,
        state["contest-service"]["config-node"],
      ).some((field) => field.path === "registration.inviteSigningKey"),
    ).toBe(false);
    expect(compositionFormErrors(composition, state)).toEqual([]);

    state["contest-service"]["config-node"]["registration.mode"] =
      "invite-only";
    expect(compositionFormErrors(composition, state)).toContain(
      "contest-service.registration.inviteSigningKey 缺少 secret 引用",
    );
    state["contest-service"]["secret-node"].secretRef =
      "vault://contest/invite-key";
    expect(compositionFormErrors(composition, state)).toEqual([]);

    const inputs = serializeCompositionInputs(composition, state);
    expect(Object.keys(inputs)).toEqual([
      "config-node",
      "secret-node",
      "resource-node",
    ]);
    expect(inputs["config-node"]).toEqual({
      config: { registration: { mode: "invite-only" } },
    });
    expect(JSON.stringify(inputs["config-node"])).not.toContain(
      "inviteSigningKey",
    );
    expect(inputs["secret-node"]).toEqual({
      secretRef: "vault://contest/invite-key",
    });

    state["contest-service"]["config-node"]["registration.mode"] = "open";
    expect(compositionFormErrors(composition, state)).toEqual([]);
    const inactive = serializeCompositionInputs(composition, state);
    expect(inactive["secret-node"]).toBeUndefined();
    expect(JSON.stringify(inactive)).not.toContain(
      "vault://contest/invite-key",
    );
  });

  it("fails closed for unresolved or out-of-set providers and keeps nodeId wire keys", () => {
    const composition = plan();
    const state = initializeCompositionState(composition);
    state["contest-service"]["config-node"]["registration.mode"] = "open";
    expect(compositionFormErrors(composition, state)).toContain(
      "contest-service.contests 尚未选择 Provider",
    );

    state["contest-service"]["resource-node"].providerId = "unknown-provider";
    expect(compositionFormErrors(composition, state)).toContain(
      "contest-service.contests Provider 不在当前候选集中",
    );

    state["contest-service"]["resource-node"].providerId = "postgres-external";
    const inputs = serializeCompositionInputs(composition, state);
    expect(inputs["resource-node"]).toEqual({
      providerId: "postgres-external",
    });
    expect(inputs["contest-service"]).toBeUndefined();
  });

  it("oneOf activates requirements only after exactly one branch matches", () => {
    const schema = {
      type: "object",
      properties: { mode: { enum: ["smtp", "local"] } },
      required: ["mode"],
      oneOf: [
        {
          properties: { mode: { const: "smtp" }, host: { type: "string" } },
          required: ["host"],
        },
        {
          properties: {
            mode: { const: "local" },
            directory: { type: "string" },
          },
          required: ["directory"],
        },
      ],
    };
    const initial = activeConfigFields(schema, {});
    expect(initial.find((field) => field.path === "host")?.required).toBe(
      false,
    );
    expect(initial.find((field) => field.path === "directory")?.required).toBe(
      false,
    );
    const smtp = activeConfigFields(schema, { mode: "smtp" });
    expect(smtp.find((field) => field.path === "host")?.required).toBe(true);
    expect(smtp.some((field) => field.path === "directory")).toBe(false);
  });

  it("dependentRequired activates a sibling requirement", () => {
    const schema = {
      type: "object",
      properties: {
        username: { type: "string" },
        passwordRef: {
          type: "string",
          writeOnly: true,
          "x-ojos-secret": true,
        },
      },
      dependentRequired: { username: ["passwordRef"] },
    };
    expect(
      activeConfigFields(schema, { username: "mailer" }).find(
        (field) => field.path === "passwordRef",
      ),
    ).toMatchObject({ required: true, secret: true });
  });
});
