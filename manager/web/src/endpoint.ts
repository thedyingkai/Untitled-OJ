export interface ParsedEndpointId {
  host: string;
  port: string;
  service: string;
}

/**
 * Endpoint ID 从右侧拆成 host、port、service。host 可能是未加方括号的 IPv6，
 * 不能按每个冒号直接 split；这与 Rust core 的两次 rsplit_once 规则一致。
 */
export function parseEndpointId(endpoint: string): ParsedEndpointId | null {
  const serviceSeparator = endpoint.lastIndexOf(":");
  if (serviceSeparator <= 0 || serviceSeparator === endpoint.length - 1) {
    return null;
  }
  const portSeparator = endpoint.lastIndexOf(":", serviceSeparator - 1);
  if (portSeparator <= 0 || portSeparator === serviceSeparator - 1) {
    return null;
  }
  const host = endpoint.slice(0, portSeparator).trim();
  const port = endpoint.slice(portSeparator + 1, serviceSeparator).trim();
  const service = endpoint.slice(serviceSeparator + 1).trim();
  return host && port && service ? { host, port, service } : null;
}
