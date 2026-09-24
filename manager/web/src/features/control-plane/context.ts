/** UI capabilities and refresh commands accepted by feature workflows. */
export interface ControlPlaneContext {
  supportsAction(action: string): boolean;
  ensureAction(action: string): boolean;
  toast(kind: "ok" | "err" | "info", text: string): void;
  refreshCore(force?: boolean): Promise<void>;
  refreshStore(refresh?: boolean): Promise<void>;
}
