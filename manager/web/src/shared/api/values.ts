export function arrayOrEmpty<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

export function textOr(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

export function numberOr(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

export function booleanOr(value: unknown, fallback = false): boolean {
  return typeof value === "boolean" ? value : fallback;
}

export function stringsOrEmpty(value: unknown): string[] {
  return arrayOrEmpty<unknown>(value).filter(
    (item): item is string => typeof item === "string",
  );
}

export function objectOrEmpty(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}
