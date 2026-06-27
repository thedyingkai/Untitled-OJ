# Judge Core Readiness

Judge Core is the first core feature module in OJOS. It provides problem catalog, submissions, judging, Worker Link, result storage and judge administration surfaces.

## Current Readiness

- Judge Core is represented as `ojos.judge-core` in Module Registry.
- Judge Core appears in Runtime Snapshot, runtime routes, runtime services and topology.
- Judge Core contributes `problem-api`, `judge-api` and `judge-worker` service/worker declarations.
- Judge Core route metadata participates in dynamic route table validation while compatibility static routes remain available.
- Judge Core disable/uninstall remains protected.

## Not GA

Judge Core is not GA. The following remain incomplete:

- True multi-machine worker deployment validation.
- Network failure and recovery validation across hosts.
- Clock drift and lease edge-case validation.
- Long soak tests.
- Full package signature and trust policy.
- Full hotplug automation for service deployment.

## Gate For Future Work

Judge Core can be used as a baseline feature module for Kernel/runtime regression, but it must not be marketed or documented as GA until the missing operational and trust work is complete.
