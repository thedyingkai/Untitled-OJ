import { afterEach } from "vitest";

export class TestResizeObserver implements ResizeObserver {
  static instances: TestResizeObserver[] = [];

  readonly observed = new Set<Element>();

  constructor(private readonly callback: ResizeObserverCallback) {
    TestResizeObserver.instances.push(this);
  }

  observe(target: Element): void {
    this.observed.add(target);
  }

  unobserve(target: Element): void {
    this.observed.delete(target);
  }

  disconnect(): void {
    this.observed.clear();
  }

  trigger(target: Element): void {
    if (!this.observed.has(target)) return;
    this.callback(
      [{ target } as ResizeObserverEntry],
      this,
    );
  }
}

Object.defineProperty(globalThis, "ResizeObserver", {
  configurable: true,
  writable: true,
  value: TestResizeObserver,
});

Object.defineProperty(HTMLElement.prototype, "offsetWidth", {
  configurable: true,
  get() {
    return Number((this as HTMLElement).dataset.testWidth ?? 176);
  },
});

Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
  configurable: true,
  get() {
    return Number((this as HTMLElement).dataset.testHeight ?? 78);
  },
});

afterEach(() => {
  TestResizeObserver.instances = [];
});
