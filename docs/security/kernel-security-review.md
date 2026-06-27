# Kernel Security Review

Date: 2026-06-27

## Dynamic Gateway Proxy

- Manifest routes reference `service_id`; they do not provide arbitrary upstream URLs.
- Gateway resolves `service_id` through trusted configuration.
- Public dynamic routes cannot claim reserved prefixes.
- Core static routes keep priority over dynamic routes.
- Unknown services and disabled routes are not proxied.

Reserved prefixes:

```text
/api/auth
/api/admin/modules
/api/admin/health
/api/health
/api/internal
/api/judge/worker
```

## Header And Auth Boundary

- Raw `Authorization` is not forwarded by default through dynamic proxy.
- Gateway forwards sanitized actor headers and internal HMAC headers.
- `public`, `user`, `admin`, `worker` and `internal` auth modes are explicit.
- Worker/internal dynamic routes are not public surfaces.

## Controlled Apply Boundary

- Gateway and Web Shell do not apply runtime plans.
- Gateway/Web/module-installer do not mount Docker socket.
- `ojosctl` / operator is the controlled apply path.
- Apply uses argv arrays, fixed compose configuration, allowlisted services, confirmation, dry-run, timeout and service locks.
- Operation history is redacted and bounded.

## Package And Manifest Boundary

- Package v1 verifies checksum integrity but does not prove publisher trust.
- Signature / trust policy remains incomplete.
- Manifest dangerous fields are rejected, including `command`, `script`, `hook`, `image`, `mount`, `host_path`, `privileged`, `cap_add`, `target_url`, secrets and token-like fields.
- Remote module market and untrusted hooks remain out of scope.

## Path Leak Boundary

- E2E scripts scan responses for internal paths and report `path_leaks`.
- Public APIs must not expose host paths, Docker socket paths, package dirs, stdout/stderr paths, checker logs, DSNs or secrets.

## Remaining Risks

- Dynamic frontend bundle security design is not complete.
- Publisher signature and trust policy are not complete.
- True multi-machine runtime apply is not complete.
- Long soak tests for Judge Core are not complete.
