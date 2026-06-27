# Module Testing Guide

Run local SDK checks before proposing a module:

```powershell
cargo run -p ojosctl -- module validate modules/sample-hello/module.yaml
cargo run -p ojosctl -- module package modules/sample-hello -o .tmp/agent/scratch/sample-hello.ojosmod
cargo run -p ojosctl -- module verify .tmp/agent/scratch/sample-hello.ojosmod
```

Run compatibility harness against Docker control plane:

```powershell
powershell -NoProfile -File scripts\e2e-module-compat.ps1 `
  -BaseUrl http://localhost:8080/api `
  -AdminUsername admin1 `
  -AdminPassword admin123 `
  -UserUsername user1 `
  -UserPassword user123
```

The harness verifies scaffold, validate, package, verify, installer dry-run/apply, enable, snapshot, menu, permission, topology, route viewer, runtime service metadata, metadata service plan blocking, disable, include-disabled inspection, uninstall dry-run, permission rejection and path leak scanning.

Temporary packages and generated scaffolds must stay under `.tmp/agent/scratch`.
