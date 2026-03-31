// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Sandbox backend integration.
//!
//! Uses the Apple Container backend to manage sandboxes as Apple Container VMs
//! via the `container` CLI, called directly from the gateway process.

pub mod apple_container;

use crate::persistence::{ObjectId, ObjectName, ObjectType};
use openshell_core::proto::Sandbox;
use std::net::IpAddr;

pub use apple_container::AppleContainerSandboxClient;

impl ObjectType for Sandbox {
    fn object_type() -> &'static str {
        "sandbox"
    }
}

impl ObjectId for Sandbox {
    fn object_id(&self) -> &str {
        &self.id
    }
}

impl ObjectName for Sandbox {
    fn object_name(&self) -> &str {
        &self.name
    }
}

// ── SandboxBackend: unified dispatch ────────────────────────────────

/// Backend-agnostic sandbox manager.
///
/// Wraps the Apple Container sandbox client for all sandbox operations.
#[derive(Clone, Debug)]
pub enum SandboxBackend {
    AppleContainer(AppleContainerSandboxClient),
}

impl SandboxBackend {
    pub fn default_image(&self) -> &str {
        let Self::AppleContainer(c) = self;
        c.default_image()
    }

    pub fn ssh_handshake_secret(&self) -> &str {
        let Self::AppleContainer(c) = self;
        c.ssh_handshake_secret()
    }

    pub const fn ssh_handshake_skew_secs(&self) -> u64 {
        let Self::AppleContainer(c) = self;
        c.ssh_handshake_skew_secs()
    }

    pub fn ssh_listen_addr(&self) -> &str {
        let Self::AppleContainer(c) = self;
        c.ssh_listen_addr()
    }

    /// Validate GPU support. Apple Container does not support GPU passthrough.
    pub async fn validate_gpu_support(&self) -> Result<(), tonic::Status> {
        Err(tonic::Status::unimplemented(
            "GPU support is not available with the Apple Container backend",
        ))
    }

    /// Create a sandbox. Returns Ok(()) on success or a tonic::Status error.
    pub async fn create(&self, sandbox: &Sandbox) -> Result<(), tonic::Status> {
        let Self::AppleContainer(c) = self;
        c.create(sandbox).await
    }

    /// Delete a sandbox. Returns true if deleted, false if not found.
    pub async fn delete(&self, name: &str) -> Result<bool, tonic::Status> {
        let Self::AppleContainer(c) = self;
        c.delete(name).await
    }

    /// Resolve the IP address of a sandbox for SSH tunnelling.
    pub async fn sandbox_ip(&self, sandbox: &Sandbox) -> Result<Option<IpAddr>, tonic::Status> {
        let Self::AppleContainer(c) = self;
        c.sandbox_ip(&sandbox.name).await
    }
}
