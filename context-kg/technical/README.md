# Technical Knowledge Domain

## Index

### Core References
- [knowledge-base.md](knowledge-base.md) — Authoritative rules, constraints, and invariants (highest priority)
- [terminology.md](terminology.md) — Domain-specific terms and definitions

### Architecture
- [arch/architecture-overview.md](arch/architecture-overview.md) — System architecture and component relationships
- [arch/dependency.md](arch/dependency.md) — Module dependencies, external service dependencies, dependency direction rules

### Module Design
- [modules/module-builder.md](modules/module-builder.md) — Builder crate: flashblock building, P2P broadcast, payload generation
- [modules/module-flashblocks.md](modules/module-flashblocks.md) — Flashblocks crate: orchestration, caching, consensus, subscriptions
- [modules/module-chainspec.md](modules/module-chainspec.md) — Chain specification: network configs, hardfork schedules
- [modules/module-intercept.md](modules/module-intercept.md) — Bridge transaction interception
- [modules/module-legacy-rpc.md](modules/module-legacy-rpc.md) — Legacy RPC routing middleware
- [modules/module-monitor.md](modules/module-monitor.md) — Full-link transaction monitoring
- [modules/module-rpc.md](modules/module-rpc.md) — X Layer RPC extensions
- [modules/module-version.md](modules/module-version.md) — Version metadata management
- [modules/module-tools.md](modules/module-tools.md) — CLI tools: import, export, genesis generation, legacy migration

### Pitfalls
- [pitfalls/data-consistency.md](pitfalls/data-consistency.md) — State consistency, cache invalidation, reorg handling
- [pitfalls/concurrency-issues.md](pitfalls/concurrency-issues.md) — Async task coordination, channel management, epoch invalidation
- [pitfalls/security-concerns.md](pitfalls/security-concerns.md) — Input validation, injection prevention, bridge security

### Core Flows
- [core-flows/block-building-flow.md](core-flows/block-building-flow.md) — Standard payload building lifecycle
- [core-flows/flashblock-flow.md](core-flows/flashblock-flow.md) — Flashblock building, broadcasting, and subscription
- [core-flows/legacy-rpc-routing-flow.md](core-flows/legacy-rpc-routing-flow.md) — Request routing to legacy vs local endpoints
- [core-flows/bridge-intercept-flow.md](core-flows/bridge-intercept-flow.md) — Bridge transaction filtering in payload builder

### APIs
- [apis/rpc-api-conventions.md](apis/rpc-api-conventions.md) — RPC interface patterns and extension points
- [apis/error-codes.md](apis/error-codes.md) — Error code definitions and handling patterns

### Conventions
- [conventions/feature-types.md](conventions/feature-types.md) — Feature flag and conditional compilation patterns
- [conventions/service-patterns.md](conventions/service-patterns.md) — Service architecture and task spawning patterns
- [conventions/common-tools.md](conventions/common-tools.md) — Shared utilities and reusable components
