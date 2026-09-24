import type { ApiCallOptions } from "../../shared/api/transport";
import { collectCursorItems } from "../../shared/api/pagination";
import { v1Request } from "../../shared/api/transport";

export const diagnosticsApi = {
  diagnostics: (options?: ApiCallOptions) =>
    collectCursorItems<Record<string, unknown>>(
      "/api/v1/diagnostics",
      (data) => data.items,
      options,
    ).then(({ items }) => items),
  createDiagnostic: (options?: ApiCallOptions) =>
    v1Request<Record<string, unknown>>(
      "POST",
      "/api/v1/diagnostics",
      {},
      options,
    ),
  diagnostic: (diagnosticId: string, options?: ApiCallOptions) =>
    v1Request<Record<string, unknown>>(
      "GET",
      `/api/v1/diagnostics/${encodeURIComponent(diagnosticId)}`,
      undefined,
      options,
    ),
  exportDiagnostic: (
    diagnosticId: string,
    format: "json" | "md",
    options?: ApiCallOptions,
  ) =>
    v1Request<Record<string, unknown>>(
      "GET",
      `/api/v1/diagnostics/${encodeURIComponent(diagnosticId)}.${format}`,
      undefined,
      options,
    ),
};
