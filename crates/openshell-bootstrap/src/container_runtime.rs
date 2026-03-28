// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Container runtime abstraction layer.
//!
//! This module defines [`RuntimeBackend`], an enum that decouples the gateway
//! bootstrap orchestration from any specific container backend (Docker, Apple
//! Container). All container lifecycle operations go through this enum, which
//! delegates to the appropriate backend implementation.

use miette::Result;
use std::collections::HashMap;
use std::path::Path;

use crate::docker::DockerRuntime;
#[cfg(target_os = "macos")]
use crate::runtime_apple::AppleContainerRuntime;

/// The type of container runtime detected on this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeType {
    /// Docker / Docker-compatible daemon (Colima, OrbStack, etc.) with k3s.
    Docker,
    /// Apple Container (macOS-native, no k3s).
    #[cfg(target_os = "macos")]
    AppleContainer,
}

impl std::fmt::Display for RuntimeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Docker => write!(f, "docker"),
            #[cfg(target_os = "macos")]
            Self::AppleContainer => write!(f, "apple_container"),
        }
    }
}

/// Result of a successful preflight check.
#[derive(Debug)]
pub struct RuntimePreflight {
    /// Human-readable version string for the runtime.
    pub version: Option<String>,
    /// The detected runtime type.
    pub runtime_type: RuntimeType,
}

/// Information about an existing gateway deployment found by the runtime.
#[derive(Debug, Clone)]
pub struct ExistingGateway {
    /// Whether the container exists.
    pub container_exists: bool,
    /// Whether the container is currently running.
    pub container_running: bool,
    /// Whether persistent storage exists.
    pub storage_exists: bool,
    /// The image used by the existing container (if any).
    pub container_image: Option<String>,
}

/// Configuration for creating a gateway container.
#[derive(Debug, Clone)]
pub struct GatewayContainerConfig {
    pub image_ref: String,
    pub extra_sans: Vec<String>,
    pub ssh_gateway_host: Option<String>,
    pub gateway_port: u16,
    pub disable_tls: bool,
    pub disable_gateway_auth: bool,
    pub registry_username: Option<String>,
    pub registry_token: Option<String>,
    pub gpu: bool,
}

/// A container that is holding a port we need.
#[derive(Debug, Clone)]
pub struct PortConflict {
    /// Name of the container holding the port.
    pub container_name: String,
    /// The host port that conflicts.
    pub host_port: u16,
}

/// Unified container runtime backend.
///
/// The orchestrator code in `lib.rs` uses this enum for all container
/// operations. Each variant delegates to the concrete backend implementation.
pub enum RuntimeBackend {
    Docker(DockerRuntime),
    #[cfg(target_os = "macos")]
    AppleContainer(AppleContainerRuntime),
}

impl RuntimeBackend {
    pub fn runtime_type(&self) -> RuntimeType {
        match self {
            Self::Docker(_) => RuntimeType::Docker,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(_) => RuntimeType::AppleContainer,
        }
    }

    pub async fn check_available(&self) -> Result<RuntimePreflight> {
        match self {
            Self::Docker(r) => r.check_available().await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.check_available().await,
        }
    }

    pub async fn ensure_image(
        &self,
        image_ref: &str,
        registry_username: Option<&str>,
        registry_token: Option<&str>,
    ) -> Result<()> {
        match self {
            Self::Docker(r) => {
                r.ensure_image(image_ref, registry_username, registry_token)
                    .await
            }
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => {
                r.ensure_image(image_ref, registry_username, registry_token)
                    .await
            }
        }
    }

    pub async fn pull_image(
        &self,
        image_ref: &str,
        registry_username: Option<&str>,
        registry_token: Option<&str>,
        on_progress: impl FnMut(String) + Send + 'static,
    ) -> Result<()> {
        match self {
            Self::Docker(r) => {
                r.pull_image(image_ref, registry_username, registry_token, on_progress)
                    .await
            }
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => {
                r.pull_image(image_ref, registry_username, registry_token, on_progress)
                    .await
            }
        }
    }

    pub async fn check_existing(&self, name: &str) -> Result<Option<ExistingGateway>> {
        match self {
            Self::Docker(r) => r.check_existing(name).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.check_existing(name).await,
        }
    }

    pub async fn create_gateway(&self, name: &str, config: &GatewayContainerConfig) -> Result<()> {
        match self {
            Self::Docker(r) => r.create_gateway(name, config).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.create_gateway(name, config).await,
        }
    }

    pub async fn start_gateway(&self, name: &str) -> Result<()> {
        match self {
            Self::Docker(r) => r.start_gateway(name).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.start_gateway(name).await,
        }
    }

    pub async fn stop_gateway(&self, name: &str) -> Result<()> {
        match self {
            Self::Docker(r) => r.stop_gateway(name).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.stop_gateway(name).await,
        }
    }

    pub async fn destroy_resources(&self, name: &str) -> Result<()> {
        match self {
            Self::Docker(r) => r.destroy_resources(name).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.destroy_resources(name).await,
        }
    }

    pub async fn ensure_network(&self, name: &str) -> Result<()> {
        match self {
            Self::Docker(r) => r.ensure_network(name).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.ensure_network(name).await,
        }
    }

    pub async fn ensure_storage(&self, name: &str) -> Result<()> {
        match self {
            Self::Docker(r) => r.ensure_storage(name).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.ensure_storage(name).await,
        }
    }

    pub async fn exec_capture(&self, name: &str, cmd: Vec<String>) -> Result<(String, i64)> {
        match self {
            Self::Docker(r) => r.exec_capture(name, cmd).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.exec_capture(name, cmd).await,
        }
    }

    pub async fn wait_for_ready(
        &self,
        name: &str,
        on_log: impl FnMut(String) + Send,
    ) -> Result<()> {
        match self {
            Self::Docker(r) => r.wait_for_ready(name, on_log).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.wait_for_ready(name, on_log).await,
        }
    }

    pub async fn fetch_recent_logs(&self, name: &str, n: usize) -> String {
        match self {
            Self::Docker(r) => r.fetch_recent_logs(name, n).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.fetch_recent_logs(name, n).await,
        }
    }

    pub async fn stream_logs<W: std::io::Write + Send>(
        &self,
        name: &str,
        follow: bool,
        lines: Option<usize>,
        writer: W,
    ) -> Result<()> {
        match self {
            Self::Docker(r) => r.stream_logs(name, follow, lines, writer).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.stream_logs(name, follow, lines, writer).await,
        }
    }

    pub async fn push_images(
        &self,
        name: &str,
        images: &[&str],
        on_log: &mut (dyn FnMut(String) + Send),
    ) -> Result<()> {
        match self {
            Self::Docker(r) => r.push_images(name, images, on_log).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.push_images(name, images, on_log).await,
        }
    }

    pub async fn check_port_conflicts(&self, name: &str, port: u16) -> Result<Vec<PortConflict>> {
        match self {
            Self::Docker(r) => r.check_port_conflicts(name, port).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.check_port_conflicts(name, port).await,
        }
    }

    pub async fn build_image(
        &self,
        dockerfile: &Path,
        tag: &str,
        context: &Path,
        args: &HashMap<String, String>,
        on_log: &mut (dyn FnMut(String) + Send),
    ) -> Result<()> {
        match self {
            Self::Docker(r) => r.build_image(dockerfile, tag, context, args, on_log).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.build_image(dockerfile, tag, context, args, on_log).await,
        }
    }

    pub async fn build_and_push_image(
        &self,
        dockerfile: &Path,
        tag: &str,
        context: &Path,
        gateway_name: &str,
        args: &HashMap<String, String>,
        on_log: &mut (dyn FnMut(String) + Send),
    ) -> Result<()> {
        match self {
            Self::Docker(r) => {
                r.build_and_push_image(dockerfile, tag, context, gateway_name, args, on_log)
                    .await
            }
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => {
                r.build_and_push_image(dockerfile, tag, context, gateway_name, args, on_log)
                    .await
            }
        }
    }

    pub async fn check_container_running(&self, name: &str) -> Result<()> {
        match self {
            Self::Docker(r) => r.check_container_running(name).await,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(r) => r.check_container_running(name).await,
        }
    }

    /// Whether this runtime uses k3s/Kubernetes for sandbox management.
    pub fn uses_kubernetes(&self) -> bool {
        match self {
            Self::Docker(_) => true,
            #[cfg(target_os = "macos")]
            Self::AppleContainer(_) => false,
        }
    }
}

/// Detect whether Apple Container is available on this system.
///
/// Runs `container system status` and checks for a "running" status.
#[cfg(target_os = "macos")]
pub fn apple_container_available() -> bool {
    let output = std::process::Command::new("container")
        .args(["system", "status"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.contains("running")
        }
        _ => false,
    }
}
