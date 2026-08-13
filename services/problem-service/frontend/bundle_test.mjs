import assert from "node:assert/strict";
import fs from "node:fs/promises";

class Element {
  constructor(tag) { this.tag = tag; this.textContent = ""; this.children = []; }
  replaceChildren(...children) { this.children = children; }
}

async function loadModule(path) {
  const source = await fs.readFile(new URL(path, import.meta.url), "utf8");
  // A data URL gives the bundle standard ESM semantics on every supported
  // Node release and deliberately provides no relative import base: a bundle
  // that is not self-contained therefore fails to load.
  globalThis.document = {createElement: (tag) => new Element(tag)};
  return import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
}

async function verify(path, operationId) {
  const namespace = await loadModule(path);
  const calls = [];
  const activation = await namespace.activate({
    client: {request: async (operation) => { calls.push(operation); return {problems: [{title: "A+B"}]}; }},
    permissions: {has: () => true, subscribe: () => ({dispose() {}})},
    theme: {current: () => ({mode: "dark", variables: {}}), subscribe: () => ({dispose() {}})},
    i18n: {locale: () => "en", translate: (key, values) => values?.count === undefined ? key : `${key}:${values.count}`, subscribe: () => ({dispose() {}})},
    logger: {debug() {}, info() {}, warn() {}, error() {}},
  });
  const root = new Element("main");
  const mounted = await activation.mount("primary", root, {});
  assert.deepEqual(calls, [operationId]);
  assert.ok(root.children.length > 0);
  mounted.dispose();
  assert.equal(root.children.length, 0);
  await activation.dispose();
}

await verify("./user/bundle.js", "listProblems");
await verify("./admin/bundle.js", "adminListProblems");
