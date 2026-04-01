// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Container runtime abstraction layer.
//!
//! This module defines [`RuntimeBackend`], which wraps the Apple Container
//! runtime. All container lifecycle operations go through this wrapper.

use miette::Result;
use std::collections::HashMap;
use std::path::Path;

use crate::runtime_apple::AppleContainerRuntime;

/// The type of container runtime detected on this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeType {
    /// Apple Container (macOS-native).
    AppleContainer,
}

impl std::fmt::Display for RuntimeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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

/// A process/container that is holding a port we need.
#[derive(Debug, Clone)]
pub struct PortConflict {
    /// Name of the process or container holding the port.
    pub container_name: String,
    /// The host port that conflicts.
    pub host_port: u16,
}

/// Unified container runtime backend.
///
/// Wraps the Apple Container runtime for all container lifecycle operations.
pub enum RuntimeBackend {
    AppleContainer(AppleContainerRuntime),
}

impl RuntimeBackend {
    pub fn runtime_type(&self) -> RuntimeType {
        match self {
            Self::AppleContainer(_) => RuntimeType::AppleContainer,
        }
    }

    pub async fn check_available(&self) -> Result<RuntimePreflight> {
        let Self::AppleContainer(r) = self;
        r.check_available().await
    }

    pub async fn ensure_image(
        &self,
        image_ref: &str,
        registry_username: Option<&str>,
        registry_token: Option<&str>,
    ) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.ensure_image(image_ref, registry_username, registry_token)
            .await
    }

    pub async fn pull_image(
        &self,
        image_ref: &str,
        registry_username: Option<&str>,
        registry_token: Option<&str>,
        on_progress: impl FnMut(String) + Send + 'static,
    ) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.pull_image(image_ref, registry_username, registry_token, on_progress)
            .await
    }

    pub async fn check_existing(&self, name: &str) -> Result<Option<ExistingGateway>> {
        let Self::AppleContainer(r) = self;
        r.check_existing(name).await
    }

    pub async fn create_gateway(&self, name: &str, config: &GatewayContainerConfig) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.create_gateway(name, config).await
    }

    pub async fn start_gateway(&self, name: &str) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.start_gateway(name).await
    }

    pub async fn stop_gateway(&self, name: &str) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.stop_gateway(name).await
    }

    pub async fn destroy_resources(&self, name: &str) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.destroy_resources(name).await
    }

    pub async fn ensure_network(&self, name: &str) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.ensure_network(name).await
    }

    pub async fn ensure_storage(&self, name: &str) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.ensure_storage(name).await
    }

    pub async fn exec_capture(&self, name: &str, cmd: Vec<String>) -> Result<(String, i64)> {
        let Self::AppleContainer(r) = self;
        r.exec_capture(name, cmd).await
    }

    pub async fn wait_for_ready(
        &self,
        name: &str,
        on_log: impl FnMut(String) + Send,
    ) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.wait_for_ready(name, on_log).await
    }

    pub async fn fetch_recent_logs(&self, name: &str, n: usize) -> String {
        let Self::AppleContainer(r) = self;
        r.fetch_recent_logs(name, n).await
    }

    pub async fn stream_logs<W: std::io::Write + Send>(
        &self,
        name: &str,
        follow: bool,
        lines: Option<usize>,
        writer: W,
    ) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.stream_logs(name, follow, lines, writer).await
    }

    pub async fn push_images(
        &self,
        name: &str,
        images: &[&str],
        on_log: &mut (dyn FnMut(String) + Send),
    ) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.push_images(name, images, on_log).await
    }

    pub async fn check_port_conflicts(&self, name: &str, port: u16) -> Result<Vec<PortConflict>> {
        let Self::AppleContainer(r) = self;
        r.check_port_conflicts(name, port).await
    }

    pub async fn build_image(
        &self,
        dockerfile: &Path,
        tag: &str,
        context: &Path,
        args: &HashMap<String, String>,
        on_log: &mut (dyn FnMut(String) + Send),
    ) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.build_image(dockerfile, tag, context, args, on_log).await
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
        let Self::AppleContainer(r) = self;
        r.build_and_push_image(dockerfile, tag, context, gateway_name, args, on_log)
            .await
    }

    pub async fn check_container_running(&self, name: &str) -> Result<()> {
        let Self::AppleContainer(r) = self;
        r.check_container_running(name).await
    }
}

/// Detect whether Apple Container is available on this system.
///
/// Runs `container system status` and checks for a "running" status.
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
