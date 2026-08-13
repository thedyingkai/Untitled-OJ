import assert from "node:assert/strict";
import fs from "node:fs/promises";
import vm from "node:vm";

class Element {
  constructor(tag) {
    this.tag = tag;
    this.textContent = "";
    this.children = [];
  }
  replaceChildren(...children) {
    this.children = children;
  }
}

async function loadModule(path) {
  const source = await fs.readFile(new URL(path, import.meta.url), "utf8");
  const context = vm.createContext({document: {createElement: (tag) => new Element(tag)}, String});
  const module = new vm.SourceTextModule(source, {context});
  await module.link(() => { throw new Error("frontend bundle must be self-contained"); });
  await module.evaluate();
  return module.namespace;
}

async function verify(path, operationId) {
  const namespace = await loadModule(path);
  const calls = [];
  const activation = await namespace.activate({
    client: {request: async (operation) => { calls.push(operation); return {items: [{title: "Cup"}]}; }},
    permissions: {has: () => true, subscribe: () => ({dispose() {}})},
    theme: {current: () => ({mode: "dark", variables: {}}), subscribe: () => ({dispose() {}})},
    i18n: {locale: () => "en", translate: (key, values) => values?.count === undefined ? key : `${key}:${values.count}`, subscribe: () => ({dispose() {}})},
    logger: {debug() {}, info() {}, warn() {}, error() {}},
  });
  const container = new Element("main");
  const mounted = await activation.mount("primary", container, {});
  assert.deepEqual(calls, [operationId]);
  assert.ok(container.children.length > 0);
  mounted.dispose();
  assert.equal(container.children.length, 0);
  await activation.dispose();
}

await verify("./user/bundle.js", "listContests");
await verify("./admin/bundle.js", "adminListContests");
