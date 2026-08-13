import assert from "node:assert/strict";
import {pathToFileURL} from "node:url";
import {resolve} from "node:path";

class FakeElement {
  constructor(tag) { this.tagName = tag; this.textContent = ""; this.children = []; }
  replaceChildren(...children) { this.children = children; }
}

globalThis.document = {createElement: (tag) => new FakeElement(tag)};

for (const [target, operation] of [["user", "getMyUserProfile"], ["admin", null]]) {
  const module = await import(pathToFileURL(resolve(`frontend/${target}/bundle.js`)).href);
  const calls = [];
  const host = {
    client: {request: async (id) => { calls.push(id); return {display_name: "Alice"}; }},
    i18n: {translate: (key) => key},
    logger: {error() {}},
  };
  const activated = await module.activate(host);
  const root = new FakeElement("main");
  const mounted = await activated.mount("surface", root);
  assert.ok(root.children.length > 0);
  if (operation) assert.deepEqual(calls, [operation]);
  mounted.dispose();
  assert.equal(root.children.length, 0);
  await activated.dispose();
}
