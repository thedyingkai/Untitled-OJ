import { mount } from "@vue/test-utils";
import { nextTick } from "vue";
import { afterEach, describe, expect, it, vi } from "vitest";

const { operationLogs, operationEvents } = vi.hoisted(() => ({
  operationLogs: vi.fn(),
  operationEvents: vi.fn(),
}));

vi.mock("../api", () => ({
  api: { operationLogs, operationEvents },
  isRequestCancelled: (error: unknown) =>
    error instanceof Error && error.name === "RequestCancelledError",
  MAX_OPERATION_LOGS: 500,
  normalizeOperationLog: (value: Record<string, unknown>) => ({
    ...value,
    operation_id: "",
    step_id: String(value.job_id ?? value.event_type ?? "runtime"),
    level: String(value.level ?? "info").toLowerCase(),
    message: String(value.message ?? ""),
    created_at: "",
  }),
}));

import OperationLogs from "./OperationLogs.vue";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe("OperationLogs polling", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("does not overlap slow polls and stops after unmount", async () => {
    vi.useFakeTimers();
    const first = deferred<{
      events: never[];
      lastEventId: string;
      retryMs: number;
    }>();
    operationEvents
      .mockReturnValueOnce(first.promise)
      .mockResolvedValue({ events: [], lastEventId: "cursor-2", retryMs: 1000 });
    const wrapper = mount(OperationLogs, {
      props: { operationId: "op-1", live: true },
    });

    expect(operationEvents).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(operationEvents).toHaveBeenCalledTimes(1);

    first.resolve({ events: [], lastEventId: "cursor-1", retryMs: 1000 });
    await Promise.resolve();
    await nextTick();
    await vi.advanceTimersByTimeAsync(999);
    expect(operationEvents).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(operationEvents).toHaveBeenCalledTimes(2);

    wrapper.unmount();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(operationEvents).toHaveBeenCalledTimes(2);
  });

  it("renders a visible error instead of silently dropping log failures", async () => {
    operationLogs.mockRejectedValue(new Error("日志服务不可用"));
    const wrapper = mount(OperationLogs, {
      props: { operationId: "op-error", live: false },
    });
    await Promise.resolve();
    await nextTick();

    expect(wrapper.text()).toContain("日志服务不可用");
    wrapper.unmount();
  });

  it("keeps the rendered event buffer bounded", async () => {
    operationLogs.mockResolvedValue(
      Array.from({ length: 500 }, (_, index) => ({
        operation_id: "op-many",
        step_id: `step-${index}`,
        level: "info",
        message: `event-${index}`,
        created_at: "",
      })),
    );
    const wrapper = mount(OperationLogs, {
      props: { operationId: "op-many", live: false },
    });
    await Promise.resolve();
    await nextTick();

    expect(wrapper.findAll(".log-line")).toHaveLength(501);
    expect(wrapper.text()).toContain("仅保留最新 500 条日志");
    wrapper.unmount();
  });
});
