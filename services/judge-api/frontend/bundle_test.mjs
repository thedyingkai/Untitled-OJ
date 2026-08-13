import assert from "node:assert/strict";
import fs from "node:fs/promises";

class FakeElement {
  constructor(tag) { this.tagName = tag; this.textContent = ""; this.children = []; }
  replaceChildren(...children) { this.children = children; }
}
async function loadModule(path) {
  const source = await fs.readFile(new URL(path, import.meta.url), "utf8");
  globalThis.document = {createElement: (tag) => new FakeElement(tag)};
  return import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
}

for (const [target, operation, response] of [
  ["user", "listSubmissions", {submissions: []}],
  ["admin", "getJudgeQueue", {pending: 0}],
]) {
  const module = await loadModule(`./${target}/bundle.js`);
  const calls = [];
  const host = {
    client: {request: async (id) => { calls.push(id); return response; }},
    permissions: {has: () => true, subscribe: () => ({dispose() {}})},
    theme: {current: () => ({mode: "dark", variables: {}}), subscribe: () => ({dispose() {}})},
    i18n: {locale: () => "en", translate: (key) => key, subscribe: () => ({dispose() {}})},
    logger: {debug() {}, info() {}, warn() {}, error() {}},
  };
  const activated = await module.activate(host);
  const root = new FakeElement("main");
  const mounted = await activated.mount("surface", root);
  assert.deepEqual(calls, [operation]);
  assert.ok(root.children.length > 0);
  mounted.dispose();
  assert.equal(root.children.length, 0);
  await activated.dispose();
}
