---
name: system-module-architecture
description: Define and review modular system architecture across web, mobile, API, and operational products. Use before creating or refactoring modules, routes, screens, workflows, or service boundaries.
---

# System Module Architecture

This standard applies to every system, regardless of stack or interface type.
It prevents a product from becoming a collection of oversized pages, implicit
contracts, and unrelated operations glued together by convenience.

## Module Boundary

- A module represents one coherent domain capability and its user journeys.
- A page, screen, API route, component folder, or service is not automatically a
  module. The boundary follows responsibility, lifecycle, ownership, and change.
- A module must declare its entities, commands, queries, states, permissions,
  dependencies, and failure modes.
- A module may contain multiple routes or screens when they share the same
  capability, but each route must have one primary intent.

## Route And Screen Topology

Separate flows when they have different intent, risk, permission, frequency, or
completion state:

- overview and operational status;
- collection, search, and filtering;
- entity detail and read-only inspection;
- creation and editing;
- configuration and policy management;
- destructive or irreversible actions;
- long-running operations and progress;
- audit, diagnostics, and support workflows.

Do not hide configuration at the bottom of an operational screen. Do not put
destructive maintenance beside routine CRUD actions. Do not solve a wrong
topology by extracting components from a giant page.

## Contracts And Boundaries

- APIs model domain commands and queries, not arbitrary upstream paths.
- Do not create catch-all proxies or forward arbitrary methods, query strings,
  headers, bodies, or `RequestInit` values to internal services.
- Every integration has a named client, explicit operations, request types,
  response types, validation, timeout, authorization, and safe errors.
- External data is parsed at the boundary and mapped to domain models before it
  enters application logic.
- Configuration is loaded through a named, typed configuration contract. Do not
  resolve arbitrary environment variable or secret names at runtime.

## Form And Workflow Rules

- Every new or modified form has a schema, field-level errors, submit state,
  server-side validation, and an explicit success or failure state.
- In TypeScript/Svelte projects, use Felte with Valibot for form state and
  runtime validation unless the stack has an established equivalent.
- Long-running and destructive commands require explicit confirmation,
  authorization, idempotency where applicable, and progress or final state.

## Review Checklist

- Can the module be described as one domain capability?
- Does every route or screen have one primary intent?
- Are configuration and maintenance separated from daily operations?
- Are irreversible actions isolated and clearly authorized?
- Are API, integration, and form boundaries explicitly typed and validated?
- Can a new developer locate the state, contract, permission, and failure path?

If any answer is no, fix the module topology before adding more features.
