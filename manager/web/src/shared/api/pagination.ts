import type { ApiCallOptions } from "./transport";
import { ApiError, v1Request } from "./transport";
import { arrayOrEmpty, textOr } from "./values";

function withCursor(path: string, cursor: string): string {
  const separator = path.includes("?") ? "&" : "?";
  const suffix = new URLSearchParams({ limit: "200" });
  if (cursor) suffix.set("cursor", cursor);
  return `${path}${separator}${suffix.toString()}`;
}

export async function collectCursorItems<T>(
  path: string,
  extract: (data: Record<string, unknown>) => unknown,
  options: ApiCallOptions = {},
): Promise<{ items: T[]; pages: Record<string, unknown>[] }> {
  const items: T[] = [];
  const pages: Record<string, unknown>[] = [];
  const seen = new Set<string>();
  let cursor = "";
  for (let page = 0; page < 100; page += 1) {
    const data = await v1Request<Record<string, unknown>>(
      "GET",
      withCursor(path, cursor),
      undefined,
      options,
    );
    pages.push(data);
    items.push(...arrayOrEmpty<T>(extract(data)));
    const next = textOr(data.next_cursor);
    if (!next) return { items, pages };
    if (seen.has(next)) {
      throw new ApiError(`集合游标重复：${path}`, 0, "INVALID_CURSOR", "", {
        cursor: next,
      });
    }
    seen.add(next);
    cursor = next;
  }
  throw new ApiError(
    `集合分页超过 100 页：${path}`,
    0,
    "PAGINATION_LIMIT_EXCEEDED",
  );
}
