import type { ApiCallOptions } from "../../shared/api/transport";
import { v1Request } from "../../shared/api/transport";
import type { LayoutState } from "../../types";

export const layoutApi = {
getLayout: (topologyId: string, options?: ApiCallOptions) =>
    v1Request<{ layout: LayoutState }>(
      "GET",
      `/api/v1/ui/layout?${new URLSearchParams({ topology_id: topologyId })}`,
      undefined,
      options,
    ).then((data) =>
      data.layout && typeof data.layout === "object" ? data.layout : {},
    ),
putLayout: (
    topologyId: string,
    layout: LayoutState,
    options?: ApiCallOptions,
  ) =>
    v1Request<{ layout: LayoutState }>(
      "PUT",
      `/api/v1/ui/layout?${new URLSearchParams({ topology_id: topologyId })}`,
      layout,
      options,
    ),
};
