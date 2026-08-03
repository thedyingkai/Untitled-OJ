import { h, nextTick } from "vue";
import { mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import FlowCanvas from "./FlowCanvas.vue";
import { TestResizeObserver } from "../test/setup";

describe("FlowCanvas", () => {
  it("settles after measuring linked nodes instead of recursively rendering", async () => {
    vi.useFakeTimers();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    let nodeRenders = 0;
    const wrapper = mount(FlowCanvas, {
      props: {
        nodes: [
          { id: "source", x: 10, y: 20 },
          { id: "target", x: 360, y: 160 },
        ],
        edges: [{ id: "edge", source: "source", target: "target" }],
      },
      slots: {
        node: ({ node }: { node: { id: string } }) => {
          nodeRenders += 1;
          return h("div", { class: "test-node" }, node.id);
        },
      },
    });

    await nextTick();
    await nextTick();
    vi.runAllTimers();
    await nextTick();

    const settledRenders = nodeRenders;
    const edge = wrapper.get(".edge-line");
    expect(edge.attributes("d")).toContain("M 186 59");
    expect(settledRenders).toBeLessThanOrEqual(6);

    const source = wrapper.findAll<HTMLElement>(".flow-node")[0].element;
    const observer = TestResizeObserver.instances[0];
    observer.trigger(source);
    observer.trigger(source);
    await nextTick();
    expect(nodeRenders).toBe(settledRenders);

    source.dataset.testWidth = "240";
    observer.trigger(source);
    await nextTick();
    expect(wrapper.get(".edge-line").attributes("d")).toContain("M 250 59");
    expect(nodeRenders).toBeGreaterThan(settledRenders);
    expect(
      consoleError.mock.calls.some((call) =>
        call.some((value) => String(value).includes("Maximum recursive updates")),
      ),
    ).toBe(false);

    wrapper.unmount();
    consoleError.mockRestore();
    vi.useRealTimers();
  });
});
