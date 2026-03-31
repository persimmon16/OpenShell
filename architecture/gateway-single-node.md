# Gateway Bootstrap Architecture

This document describes how OpenShell bootstraps the gateway as a native macOS process using Apple Container for sandbox management.

## Goals and Scope

- Provide a single bootstrap flow through `openshell-bootstrap` for gateway lifecycle.
- Use Apple Container as the only runtime dependency.
- Support idempotent `deploy` behavior (safe to re-run).
- Persist gateway access artifacts (metadata, mTLS certs) in the local XDG config directory.
- Track the active gateway so most CLI commands resolve their target automatically.

Out of scope:

- Multi-node orchestration.

## Apple Container Path

The gateway runs as a native process (macOS 15+). Sandboxes are Apple Container VMs managed via the `container` CLI.

### Bootstrap sequence

1. **Detect Apple Container**: Verify that the `container` CLI is available on `PATH`.
2. **Locate `openshell-server` binary**: Find or build the gateway server binary.
3. **Generate PKI**: Create mTLS certificates and store them at `~/.openshell/gateways/<name>/pki/`.
4. **Start gateway as background process**: Launch `openshell-server` as a native macOS process.
5. **Write PID file**: Record the process ID at `~/.openshell/gateways/<name>/gateway.pid`.
6. **TCP health probe**: Poll the gateway's listen port until it accepts connections.

### Sandbox management

Sandboxes are Apple Container VMs managed via the `container` CLI. The gateway invokes `container` commands to create, start, stop, and destroy sandbox VMs over the local vmnet network. No Kubernetes pods, Helm charts, or k3s components are involved.

### File system layout

```
~/.openshell/gateways/<name>/
  gateway.pid          # PID of the running gateway process
  pki/                 # mTLS certificates (ca.crt, server.crt, server.key)
  logs/
    server.log         # Gateway process stdout/stderr
```

### Implementation

- `crates/openshell-bootstrap/src/runtime_apple.rs` -- Apple Container runtime backend
- `crates/openshell-bootstrap/src/lib.rs` -- runtime dispatch
- `crates/openshell-bootstrap/src/runtime_apple.rs` -- Apple Container runtime backend
