// Timers and in-flight requests belong to one Pinia store, not the module.
// Keep browser resources outside serializable/reactive product state.
class OrchestratorRuntime {
  pollTimer: ReturnType<typeof setInterval> | null = null;
  layoutTimer: ReturnType<typeof setTimeout> | null = null;
  visibilityHandler: (() => void) | null = null;
  coreRefresh: Promise<void> | null = null;
  coreRefreshGeneration = 0;
  coreRefreshController: AbortController | null = null;
  storeRefreshController: AbortController | null = null;
  layoutLoadController: AbortController | null = null;
  layoutSaveController: AbortController | null = null;
  toastTimers = new Map<number, ReturnType<typeof setTimeout>>();
  toastSeq = 1;
}

const runtimes = new WeakMap<object, OrchestratorRuntime>();

export function runtimeFor(owner: object): OrchestratorRuntime {
  let runtime = runtimes.get(owner);
  if (!runtime) {
    runtime = new OrchestratorRuntime();
    runtimes.set(owner, runtime);
  }
  return runtime;
}
