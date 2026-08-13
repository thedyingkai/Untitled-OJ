import type {
  CompositionNode,
  CompositionPlan,
  CompositionProviderCandidate,
} from "./types";

export type CompositionFormValue = string | boolean;
export type CompositionFormState = Record<
  string,
  Record<string, Record<string, CompositionFormValue>>
>;

export interface ConfigField {
  path: string;
  title: string;
  description: string;
  type: string;
  required: boolean;
  secret: boolean;
  allowedValues: unknown[];
  defaultValue?: CompositionFormValue;
}

type JsonObject = Record<string, unknown>;

function object(value: unknown): JsonObject {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as JsonObject)
    : {};
}

function strings(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string")
    : [];
}

function propertySchemas(schema: JsonObject): Map<string, JsonObject> {
  const result = new Map<string, JsonObject>();
  const visit = (candidate: JsonObject, prefix = "") => {
    for (const [name, declaration] of Object.entries(
      object(candidate.properties),
    )) {
      const path = prefix ? `${prefix}.${name}` : name;
      const property = object(declaration);
      if (
        property.type === "object" ||
        Object.keys(object(property.properties)).length
      ) {
        visit(property, path);
      } else {
        const previous = result.get(path);
        result.set(
          path,
          previous
            ? {
                ...previous,
                ...property,
                writeOnly:
                  previous.writeOnly === true || property.writeOnly === true,
                "x-ojos-secret":
                  previous["x-ojos-secret"] === true ||
                  property["x-ojos-secret"] === true,
              }
            : property,
        );
      }
    }
    for (const keyword of ["allOf", "anyOf", "oneOf"] as const) {
      for (const branch of Array.isArray(candidate[keyword])
        ? candidate[keyword]
        : []) {
        visit(object(branch), prefix);
      }
    }
    for (const keyword of ["if", "then", "else"] as const) {
      if (candidate[keyword]) visit(object(candidate[keyword]), prefix);
    }
  };
  visit(schema);
  return result;
}

function schemaAtPath(schema: JsonObject, path: string): JsonObject {
  let cursor = schema;
  for (const segment of path.split(".")) {
    cursor = object(object(cursor.properties)[segment]);
  }
  return cursor;
}

function setPath(root: JsonObject, path: string, value: unknown) {
  const segments = path.split(".");
  let cursor = root;
  segments.forEach((segment, index) => {
    if (index === segments.length - 1) {
      cursor[segment] = value;
      return;
    }
    cursor = object(cursor[segment]);
    rootAtPath(root, segments.slice(0, index + 1), cursor);
  });
}

function rootAtPath(root: JsonObject, segments: string[], value: JsonObject) {
  let cursor = root;
  segments.forEach((segment, index) => {
    if (index === segments.length - 1) {
      cursor[segment] = value;
    } else {
      const child = object(cursor[segment]);
      cursor[segment] = child;
      cursor = child;
    }
  });
}

function present(value: unknown): boolean {
  return value !== undefined && value !== null && value !== "";
}

function coerce(value: CompositionFormValue, schema: JsonObject): unknown {
  if (typeof value === "boolean") return value;
  if (schema.type === "boolean") {
    if (value === "true") return true;
    if (value === "false") return false;
  }
  if (schema.type === "integer") {
    const parsed = Number.parseInt(value, 10);
    return Number.isFinite(parsed) ? parsed : value;
  }
  if (schema.type === "number") {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : value;
  }
  return value;
}

function instanceFromState(
  schema: JsonObject,
  values: Record<string, CompositionFormValue>,
  secretPaths: string[] = [],
): JsonObject {
  const definitions = propertySchemas(schema);
  const instance: JsonObject = {};
  for (const [path, value] of Object.entries(values)) {
    if (!present(value)) continue;
    const declaration = definitions.get(path);
    if (!declaration) continue;
    setPath(instance, path, coerce(value, declaration));
  }
  for (const path of secretPaths) setPath(instance, path, "opaque");
  return instance;
}

function schemaMatches(instance: unknown, schemaValue: unknown): boolean {
  const schema = object(schemaValue);
  if (schema.const !== undefined && instance !== schema.const) return false;
  if (
    Array.isArray(schema.enum) &&
    !schema.enum.some((item) => item === instance)
  )
    return false;
  if (
    schema.type === "object" &&
    (instance === null || typeof instance !== "object")
  ) {
    return false;
  }
  if (schema.type === "string" && typeof instance !== "string") return false;
  if (schema.type === "boolean" && typeof instance !== "boolean") return false;
  if (schema.type === "number" && typeof instance !== "number") return false;
  if (schema.type === "integer" && !Number.isInteger(instance)) return false;
  for (const key of strings(schema.required)) {
    if (!present(object(instance)[key])) return false;
  }
  for (const [name, declaration] of Object.entries(object(schema.properties))) {
    const child = object(instance)[name];
    if (child !== undefined && !schemaMatches(child, declaration)) return false;
  }
  if (
    Array.isArray(schema.allOf) &&
    !schema.allOf.every((branch) => schemaMatches(instance, branch))
  ) {
    return false;
  }
  if (
    Array.isArray(schema.anyOf) &&
    !schema.anyOf.some((branch) => schemaMatches(instance, branch))
  ) {
    return false;
  }
  if (Array.isArray(schema.oneOf)) {
    if (
      schema.oneOf.filter((branch) => schemaMatches(instance, branch))
        .length !== 1
    )
      return false;
  }
  if (schema.not && schemaMatches(instance, schema.not)) return false;
  return true;
}

function branchCompatible(instance: unknown, schemaValue: unknown): boolean {
  const schema = object(schemaValue);
  if (
    schema.const !== undefined &&
    present(instance) &&
    instance !== schema.const
  )
    return false;
  if (
    Array.isArray(schema.enum) &&
    present(instance) &&
    !schema.enum.some((item) => item === instance)
  ) {
    return false;
  }
  for (const [name, declaration] of Object.entries(object(schema.properties))) {
    const child = object(instance)[name];
    if (present(child) && !branchCompatible(child, declaration)) return false;
  }
  return true;
}

function fieldFromSchema(
  path: string,
  schema: JsonObject,
  required: boolean,
): ConfigField {
  const allowedValues = Array.isArray(schema.enum)
    ? [...schema.enum]
    : schema.const !== undefined
      ? [schema.const]
      : [];
  const defaultValue =
    typeof schema.default === "string" || typeof schema.default === "boolean"
      ? schema.default
      : typeof schema.default === "number"
        ? String(schema.default)
        : undefined;
  return {
    path,
    title:
      typeof schema.title === "string"
        ? schema.title
        : (path.split(".").at(-1) ?? path),
    description:
      typeof schema.description === "string" ? schema.description : "",
    type: typeof schema.type === "string" ? schema.type : "string",
    required,
    secret: schema.writeOnly === true && schema["x-ojos-secret"] === true,
    allowedValues,
    ...(defaultValue !== undefined ? { defaultValue } : {}),
  };
}

function collectActiveFields(
  schemaValue: unknown,
  instance: unknown,
  prefix: string,
  result: Map<string, ConfigField>,
  requirementsActive = true,
) {
  const schema = object(schemaValue);
  const required = new Set(requirementsActive ? strings(schema.required) : []);
  for (const name of required) {
    const path = prefix ? `${prefix}.${name}` : name;
    const previous = result.get(path);
    const declaration = object(object(schema.properties)[name]);
    if (previous) {
      result.set(path, { ...previous, required: true });
    } else if (
      Object.keys(declaration).length &&
      declaration.type !== "object" &&
      !Object.keys(object(declaration.properties)).length
    ) {
      result.set(path, fieldFromSchema(path, declaration, true));
    }
  }
  if (requirementsActive) {
    for (const [trigger, dependencies] of Object.entries(
      object(schema.dependentRequired),
    )) {
      if (present(object(instance)[trigger])) {
        strings(dependencies).forEach((name) => required.add(name));
      }
    }
  }
  for (const [name, declarationValue] of Object.entries(
    object(schema.properties),
  )) {
    const declaration = object(declarationValue);
    const path = prefix ? `${prefix}.${name}` : name;
    const child = object(instance)[name];
    if (
      declaration.type === "object" ||
      Object.keys(object(declaration.properties)).length
    ) {
      const childActive =
        required.has(name) || Object.keys(object(child)).length > 0;
      collectActiveFields(
        declaration,
        child,
        path,
        result,
        requirementsActive && childActive,
      );
      continue;
    }
    const candidate = fieldFromSchema(
      path,
      declaration,
      requirementsActive && required.has(name),
    );
    const previous = result.get(path);
    result.set(
      path,
      previous
        ? {
            ...previous,
            ...candidate,
            required: previous.required || candidate.required,
            secret: previous.secret || candidate.secret,
            allowedValues: previous.allowedValues.length
              ? previous.allowedValues
              : candidate.allowedValues,
          }
        : candidate,
    );
  }
  for (const branch of Array.isArray(schema.allOf) ? schema.allOf : []) {
    collectActiveFields(branch, instance, prefix, result, requirementsActive);
  }
  if (schema.if) {
    const branch = schemaMatches(instance, schema.if)
      ? schema.then
      : schema.else;
    if (branch)
      collectActiveFields(branch, instance, prefix, result, requirementsActive);
  }
  for (const name of required) {
    const path = prefix ? `${prefix}.${name}` : name;
    const field = result.get(path);
    if (field) result.set(path, { ...field, required: true });
  }
  const forbidden = object(schema.not);
  for (const name of strings(forbidden.required)) {
    result.delete(prefix ? `${prefix}.${name}` : name);
  }
  if (Array.isArray(schema.oneOf)) {
    const completeMatches = schema.oneOf.filter((branch) =>
      schemaMatches(instance, branch),
    );
    const matches =
      completeMatches.length === 1
        ? completeMatches
        : schema.oneOf.filter((branch) => branchCompatible(instance, branch));
    if (matches.length === 1) {
      collectActiveFields(
        matches[0],
        instance,
        prefix,
        result,
        requirementsActive,
      );
    } else {
      // Before a discriminator produces one valid branch, expose its possible
      // fields but never require mutually exclusive branch inputs.
      for (const branch of schema.oneOf) {
        collectActiveFields(branch, instance, prefix, result, false);
      }
    }
  }
}

export function activeConfigFields(
  schema: Record<string, unknown>,
  values: Record<string, CompositionFormValue>,
  secretPaths: string[] = [],
): ConfigField[] {
  const instance = instanceFromState(schema, values, secretPaths);
  const fields = new Map<string, ConfigField>();
  collectActiveFields(schema, instance, "", fields);
  return [...fields.values()].sort((left, right) =>
    left.path.localeCompare(right.path),
  );
}

export function nodeValues(
  state: CompositionFormState,
  node: CompositionNode,
): Record<string, CompositionFormValue> {
  return state[node.serviceId]?.[node.nodeId] ?? {};
}

export function initializeCompositionState(
  plan: CompositionPlan,
  previous: CompositionFormState = {},
): CompositionFormState {
  const next: CompositionFormState = {};
  for (const node of plan.nodes) {
    const values = { ...(previous[node.serviceId]?.[node.nodeId] ?? {}) };
    for (const declaration of node.unresolvedInputs) {
      if (values[declaration.key] === undefined) values[declaration.key] = "";
    }
    if (node.kind === "config" && node.schema) {
      for (const field of activeConfigFields(node.schema, values)) {
        if (
          !field.secret &&
          values[field.path] === undefined &&
          field.defaultValue !== undefined
        ) {
          values[field.path] = field.defaultValue;
        }
      }
    }
    next[node.serviceId] ??= {};
    next[node.serviceId][node.nodeId] = values;
  }
  return next;
}

export function compositionServices(plan: CompositionPlan): string[] {
  return [...new Set(plan.nodes.map((node) => node.serviceId))].sort();
}

export function nodesForService(
  plan: CompositionPlan,
  serviceId: string,
): CompositionNode[] {
  return plan.nodes.filter((node) => node.serviceId === serviceId);
}

export function providerCandidate(
  node: CompositionNode,
  providerId: string,
): CompositionProviderCandidate | undefined {
  return node.provider?.candidates.find(
    (candidate) => candidate.providerId === providerId,
  );
}

function configNodeForService(
  plan: CompositionPlan,
  serviceId: string,
): CompositionNode | undefined {
  return plan.nodes.find(
    (node) => node.serviceId === serviceId && node.kind === "config",
  );
}

function declaredSecretFields(node: CompositionNode): ConfigField[] {
  if (!node.schema) return [];
  const definitions = propertySchemas(node.schema);
  return [...definitions.entries()]
    .map(([path, declaration]) => fieldFromSchema(path, declaration, false))
    .filter((field) => field.secret);
}

function suppliedSecretPaths(
  plan: CompositionPlan,
  serviceId: string,
  state: CompositionFormState,
): string[] {
  return plan.nodes
    .filter(
      (node) =>
        node.serviceId === serviceId && node.kind === "secret" && node.name,
    )
    .filter((node) => {
      const declaration = node.unresolvedInputs.find(
        (input) => input.valueType === "secret-ref",
      );
      return declaration && present(nodeValues(state, node)[declaration.key]);
    })
    .map((node) => node.name as string);
}

export function activeNodeConfigFields(
  plan: CompositionPlan,
  node: CompositionNode,
  state: CompositionFormState,
): ConfigField[] {
  if (!node.schema) return [];
  return activeConfigFields(
    node.schema,
    nodeValues(state, node),
    suppliedSecretPaths(plan, node.serviceId, state),
  );
}

export function secretNodeState(
  plan: CompositionPlan,
  node: CompositionNode,
  state: CompositionFormState,
): { active: boolean; required: boolean } {
  if (node.kind !== "secret" || !node.name)
    return { active: true, required: !!node.required };
  const configNode = configNodeForService(plan, node.serviceId);
  if (!configNode?.schema) return { active: true, required: !!node.required };
  const declared = declaredSecretFields(configNode).some(
    (field) => field.path === node.name,
  );
  if (!declared) return { active: true, required: !!node.required };
  const active = activeConfigFields(
    configNode.schema,
    nodeValues(state, configNode),
    suppliedSecretPaths(plan, node.serviceId, state),
  ).find((field) => field.path === node.name && field.secret);
  return { active: !!active, required: !!active?.required };
}

function validSecretReference(value: string): boolean {
  return /^[a-z0-9][a-z0-9+.-]*:\/\/\S+$/.test(value) && value.length <= 2048;
}

export function compositionFormErrors(
  plan: CompositionPlan,
  state: CompositionFormState,
): string[] {
  const errors: string[] = [];
  for (const node of plan.nodes) {
    const values = nodeValues(state, node);
    for (const declaration of node.unresolvedInputs) {
      if (declaration.valueType === "json-object") continue;
      if (declaration.valueType === "secret-ref") {
        const secret = secretNodeState(plan, node, state);
        if (!secret.active) continue;
        const reference = String(values[declaration.key] ?? "").trim();
        if (secret.required && !reference)
          errors.push(
            `${node.serviceId}.${node.name ?? declaration.key} 缺少 secret 引用`,
          );
        if (reference && !validSecretReference(reference))
          errors.push(
            `${node.serviceId}.${node.name ?? declaration.key} 必须是 URI-like secret 引用`,
          );
        continue;
      }
      const value = String(values[declaration.key] ?? "").trim();
      if (declaration.required && !value)
        errors.push(
          `${node.serviceId}.${node.name ?? declaration.key} 尚未选择 Provider`,
        );
      if (value && !declaration.allowedValues.includes(value))
        errors.push(
          `${node.serviceId}.${node.name ?? declaration.key} Provider 不在当前候选集中`,
        );
      if (declaration.required && declaration.allowedValues.length === 0)
        errors.push(
          `${node.serviceId}.${node.name ?? declaration.key} 当前没有可解析的 Provider 候选`,
        );
    }
    if (node.kind === "config" && node.schema) {
      for (const field of activeConfigFields(
        node.schema,
        values,
        suppliedSecretPaths(plan, node.serviceId, state),
      )) {
        if (!field.secret && field.required && !present(values[field.path])) {
          errors.push(`${node.serviceId}.${field.path} 是活动分支的必填配置`);
        }
      }
    }
  }
  return [...new Set(errors)];
}

function serializeConfig(
  plan: CompositionPlan,
  node: CompositionNode,
  state: CompositionFormState,
  values: Record<string, CompositionFormValue>,
): JsonObject {
  const output: JsonObject = {};
  if (!node.schema) return output;
  for (const field of activeNodeConfigFields(plan, node, state)) {
    if (field.secret || !present(values[field.path])) continue;
    const declaration = {
      ...(propertySchemas(node.schema).get(field.path) ?? {}),
      ...schemaAtPath(node.schema, field.path),
      type: field.type,
    };
    setPath(output, field.path, coerce(values[field.path], declaration));
  }
  return output;
}

export function serializeCompositionInputs(
  plan: CompositionPlan,
  state: CompositionFormState,
): Record<string, Record<string, unknown>> {
  const result: Record<string, Record<string, unknown>> = {};
  for (const node of plan.nodes) {
    const values = nodeValues(state, node);
    const output: Record<string, unknown> = {};
    for (const declaration of node.unresolvedInputs) {
      if (declaration.valueType === "json-object") {
        output[declaration.key] = serializeConfig(plan, node, state, values);
      } else if (declaration.valueType === "secret-ref") {
        if (!secretNodeState(plan, node, state).active) continue;
        const reference = String(values[declaration.key] ?? "").trim();
        if (reference) output[declaration.key] = reference;
      } else {
        const value = String(values[declaration.key] ?? "").trim();
        if (value) output[declaration.key] = value;
      }
    }
    if (Object.keys(output).length) result[node.nodeId] = output;
  }
  return result;
}
