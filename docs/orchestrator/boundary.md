# Orchestrator Boundary

OJOS Orchestrator is a service orchestrator, not the OJ business backend and not the Gateway frontend.

## Gateway

Gateway is a service. It handles business traffic, auth middleware, request routing, unified errors, and health reporting.

Gateway does not install services, manage endpoints or links, mutate topology, execute operations, or become the control plane.

## Gateway Frontend

Gateway frontend is the OJ site UI: problems, submissions, judging results, and ordinary administration views.

Gateway frontend does not install services, manage endpoints or links, mutate topology, execute operations, or act as Orchestrator.

## Orchestrator Daemon

Orchestrator daemon is the HTTP API entry point for Orchestrator.

It may:

```text
read ORCHESTRATOR_DATABASE_URL or use local memory store context
expose service release, service, endpoint, link, operation, topology, log, and diagnostic APIs
convert write requests into core ActionRequest values
delegate execution to OrchestratorActionDispatcher
read operation state, operation logs, topology, and diagnostic reports
```

It must not proxy OJ business traffic, serve Gateway frontend pages, execute arbitrary shell, bypass the GUI/TUI core action schema, or introduce an extra runtime instance object.

## Root Role

Root is an Orchestrator runtime role, not a separate rootd program. Node and standalone are also roles of the same Orchestrator program.

Root information is represented by configuration, authority policy, and topology start points:

```text
topology.root_host
topology.root_endpoint
authority.root_host
authority.root_endpoint
authority.exposure_policy
```

## OJ Business Boundary

Problems, submissions, contests, users, permissions business logic, announcements, training, clarifications, printing, and rankings belong inside managed services, not inside Orchestrator core.
