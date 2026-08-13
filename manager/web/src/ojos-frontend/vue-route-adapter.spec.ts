import { describe, expect, it, vi } from "vitest";
import { createMemoryHistory, createRouter } from "vue-router";
import { createVueRouteAdapter } from "./vue-route-adapter";

function route(path: string) {
  return { id: "contest.list", path, title: "Contests", menu: true, order: 1 };
}

const view = { mount: vi.fn(async () => undefined), unmount: vi.fn() };

describe("Vue contribution route adapter", () => {
  it("rejects Shell and cross-module path collisions", () => {
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [{ path: "/static", name: "static", component: { template: "<div />" } }],
    });
    const adapter = createVueRouteAdapter(router);
    expect(() => adapter.validate("contest.admin", [route("/static")])).toThrow(/conflicts with the Shell/);
    const disposable = adapter.register("contest.admin", route("/contests"), view);
    expect(() => adapter.validate("problem.admin", [route("/contests")])).toThrow(/owned by contest.admin/);
    disposable.dispose();
  });

  it("permits staged same-module replacement and removes revisions independently", () => {
    const router = createRouter({ history: createMemoryHistory(), routes: [] });
    const adapter = createVueRouteAdapter(router);
    const prior = adapter.register("contest.admin", route("/contests"), view);
    expect(() => adapter.validate("contest.admin", [route("/contests")])).not.toThrow();
    const candidate = adapter.register("contest.admin", route("/contests"), view);
    prior.dispose();
    expect(router.getRoutes().filter((item) => item.path === "/contests")).toHaveLength(1);
    candidate.dispose();
    expect(router.getRoutes().filter((item) => item.path === "/contests")).toHaveLength(0);
  });
});
