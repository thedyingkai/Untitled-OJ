import type { ApiCallOptions } from "../../shared/api/transport";
import { collectCursorItems } from "../../shared/api/pagination";
import { v1Request } from "../../shared/api/transport";

export const catalogApi = {
  catalogs: (options?: ApiCallOptions) =>
    collectCursorItems<Record<string, unknown>>(
      "/api/v1/store/catalogs",
      (data) => data.items,
      options,
    ).then(({ items }) => items),
  registerCatalog: (
    source: {
      id: string;
      url: string;
      required_key_id: string;
      auth_secret_ref?: string;
      public_key?: string;
    },
    options?: ApiCallOptions,
  ) =>
    v1Request<Record<string, unknown>>(
      "POST",
      "/api/v1/store/catalogs",
      source,
      options,
    ),
  removeCatalog: (sourceId: string, options?: ApiCallOptions) =>
    v1Request<Record<string, unknown>>(
      "DELETE",
      `/api/v1/store/catalogs/${encodeURIComponent(sourceId)}`,
      {},
      options,
    ),
};
