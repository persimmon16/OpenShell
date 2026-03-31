<!--
  SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
  SPDX-License-Identifier: Apache-2.0
-->

# Support Matrix

This page lists the platform, software, runtime, and kernel requirements for running OpenShell.

## Supported Platforms

| Platform                | Architecture          | Status    |
| ----------------------- | --------------------- | --------- |
| macOS (Apple Container) | Apple Silicon (arm64)  | Supported |

## Software Prerequisites

The following software must be installed on the host before using the OpenShell CLI:

| Component | Minimum Version | Notes |
| --------- | --------------- | ----- |
| [Apple Container](https://github.com/apple/container) | 0.10.0 | Requires macOS 15 (Sequoia) or later. Install with `brew install container`. |

## Sandbox Runtime Versions

Sandbox container images are maintained in the [openshell-community](https://github.com/nvidia/openshell-community) repository. Refer to that repository for the current list of installed components and their versions.

## Container Images

The gateway runs as a native process. Sandbox images are pulled directly by Apple Container.

Sandbox images are maintained in the [openshell-community](https://github.com/nvidia/openshell-community) repository.

To override the default sandbox image registry, set the following environment variable:

| Variable                       | Purpose                                             |
| ------------------------------ | --------------------------------------------------- |
| `OPENSHELL_COMMUNITY_REGISTRY` | Override the registry for community sandbox images. |

## Kernel Requirements

Sandbox isolation is provided by the macOS Virtualization.framework hypervisor. Each sandbox runs as a lightweight Linux VM with its own kernel. Landlock and seccomp enforcement runs inside the VM kernel, not on the host.

## Agent Compatibility

For the full list of supported agents and their default policy coverage, refer to the {doc}`../about/supported-agents` page.
