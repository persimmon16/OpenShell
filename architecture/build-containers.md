# Container Images

The gateway runs as a native process. Sandbox images are OCI container images pulled directly by Apple Container.

## Sandbox Images

Sandbox images are **not built in this repository**. They are maintained in the [openshell-community](https://github.com/nvidia/openshell-community) repository and pulled from `ghcr.io/nvidia/openshell-community/sandboxes/` at runtime.

The default sandbox image is `ghcr.io/nvidia/openshell-community/sandboxes/base:latest`. To use a named community sandbox:

```bash
openshell sandbox create --from <name>
```

This pulls `ghcr.io/nvidia/openshell-community/sandboxes/<name>:latest`.

## Local Development

Use `mise` commands for development builds. Apple Container handles sandbox images natively.

```bash
mise run sandbox     # Create a sandbox on the running gateway
```

