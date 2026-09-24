

export interface CatalogSourceRow {
  id: string;
  url: string;
  required_key_id: string;
  auth_secret_ref: string;
  enabled: boolean;
}

export function normalizeCatalogSource(
  value: Record<string, unknown>,
): CatalogSourceRow | null {
  const id = typeof value.id === "string" ? value.id.trim() : "";
  const url = typeof value.url === "string" ? value.url.trim() : "";
  const requiredKeyId =
    typeof value.required_key_id === "string"
      ? value.required_key_id.trim()
      : "";
  if (!id || !url || !requiredKeyId) return null;
  return {
    id,
    url,
    required_key_id: requiredKeyId,
    auth_secret_ref:
      typeof value.auth_secret_ref === "string" ? value.auth_secret_ref : "",
    enabled: value.enabled !== false,
  };
}

export function isCanonicalEd25519PublicKey(value: string): boolean {
  if (!/^[A-Za-z0-9+/]{43}=$/.test(value)) return false;
  try {
    const raw = window.atob(value);
    return raw.length === 32 && window.btoa(raw) === value;
  } catch {
    return false;
  }
}
