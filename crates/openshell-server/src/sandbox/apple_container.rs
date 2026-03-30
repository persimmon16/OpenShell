// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Apple Container sandbox backend.
//!
//! Manages sandbox containers by calling the `container` CLI directly from
//! the gateway process running natively on macOS. No Kubernetes, no bridge
//! daemon — the gateway is a macOS process that creates sandbox VMs via the
//! Apple Container Virtualization.framework.

use openshell_core::proto::{Sandbox, SandboxPhase, SandboxSpec};
use std::net::IpAddr;
use tracing::{debug, info, warn};

const SANDBOX_PREFIX: &str = "openshell-sandbox-";

/// Sandbox client using the Apple Container CLI directly.
///
/// Runs `container create/start/stop/delete/list` as subprocesses. Each sandbox
/// is a lightweight Apple Container VM with its own Linux kernel and vmnet IP.
#[derive(Clone, Debug)]
pub struct AppleContainerSandboxClient {
    default_image: String,
    ssh_listen_addr: String,
    ssh_handshake_secret: String,
    ssh_handshake_skew_secs: u64,
    grpc_endpoint: String,
}

impl AppleContainerSandboxClient {
    pub fn new(
        default_image: String,
        grpc_endpoint: String,
        ssh_listen_addr: String,
        ssh_handshake_secret: String,
        ssh_handshake_skew_secs: u64,
    ) -> Self {
        Self {
            default_image,
            grpc_endpoint,
            ssh_listen_addr,
            ssh_handshake_secret,
            ssh_handshake_skew_secs,
        }
    }

    pub fn default_image(&self) -> &str {
        &self.default_image
    }

    pub fn ssh_listen_addr(&self) -> &str {
        &self.ssh_listen_addr
    }

    pub fn ssh_handshake_secret(&self) -> &str {
        &self.ssh_handshake_secret
    }

    pub const fn ssh_handshake_skew_secs(&self) -> u64 {
        self.ssh_handshake_skew_secs
    }

    /// Create a sandbox container via `container create` + `container start`.
    pub async fn create(&self, sandbox: &Sandbox) -> Result<(), tonic::Status> {
        let spec = sandbox
            .spec
            .as_ref()
            .ok_or_else(|| tonic::Status::invalid_argument("sandbox spec is required"))?;

        let image = spec
            .template
            .as_ref()
            .map(|t| &t.image)
            .filter(|i| !i.is_empty())
            .cloned()
            .unwrap_or_else(|| self.default_image.clone());

        let container_name = format!("{SANDBOX_PREFIX}{}", sandbox.name);

        // Ensure the image is available locally.
        self.ensure_image(&image).await?;

        // Build the create command.
        let mut args = vec![
            "create".to_string(),
            "--name".to_string(),
            container_name.clone(),
            "-d".to_string(),
        ];

        // Environment variables for the sandbox.
        let env_pairs = self.sandbox_env_vars(sandbox, spec);
        for (key, value) in &env_pairs {
            args.push("-e".to_string());
            args.push(format!("{key}={value}"));
        }

        args.push(image.clone());

        debug!(sandbox = %sandbox.name, image = %image, "creating sandbox container");
        let output = tokio::process::Command::new("container")
            .args(&args)
            .output()
            .await
            .map_err(|e| tonic::Status::internal(format!("failed to run container CLI: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Check for name conflict (container already exists).
            if stderr.contains("already exists") || stderr.contains("already in use") {
                return Err(tonic::Status::already_exists(format!(
                    "sandbox container '{container_name}' already exists"
                )));
            }
            return Err(tonic::Status::internal(format!(
                "failed to create sandbox container: {stderr}"
            )));
        }

        // Start the container.
        let start_output = tokio::process::Command::new("container")
            .args(["start", &container_name])
            .output()
            .await
            .map_err(|e| tonic::Status::internal(format!("failed to start container: {e}")))?;

        if !start_output.status.success() {
            let stderr = String::from_utf8_lossy(&start_output.stderr);
            return Err(tonic::Status::internal(format!(
                "failed to start sandbox container: {stderr}"
            )));
        }

        info!(sandbox = %sandbox.name, "created and started sandbox container");
        Ok(())
    }

    /// Delete a sandbox container via `container stop` + `container delete`.
    pub async fn delete(&self, name: &str) -> Result<bool, tonic::Status> {
        let container_name = format!("{SANDBOX_PREFIX}{name}");

        // Stop first (ignore errors — may already be stopped).
        let _ = tokio::process::Command::new("container")
            .args(["stop", &container_name])
            .output()
            .await;

        let output = tokio::process::Command::new("container")
            .args(["delete", &container_name])
            .output()
            .await
            .map_err(|e| tonic::Status::internal(format!("failed to run container CLI: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("not found") || stderr.contains("does not exist") {
                return Ok(false);
            }
            return Err(tonic::Status::internal(format!(
                "failed to delete sandbox container: {stderr}"
            )));
        }

        info!(sandbox = %name, "deleted sandbox container");
        Ok(true)
    }

    /// Get the vmnet IP address of a sandbox container.
    ///
    /// Apple Container VMs get routable IPs on the vmnet bridge (192.168.65.x).
    /// The gateway, running natively on macOS, can reach these IPs directly.
    pub async fn sandbox_ip(&self, name: &str) -> Result<Option<IpAddr>, tonic::Status> {
        let container_name = format!("{SANDBOX_PREFIX}{name}");
        let containers = self.list_containers_json().await?;

        for c in &containers {
            let id = c
                .pointer("/configuration/id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if id == container_name {
                // Extract IP from networks[0].ipv4Address (format: "192.168.65.x/24")
                if let Some(ip_str) = c
                    .pointer("/networks/0/ipv4Address")
                    .and_then(|v| v.as_str())
                {
                    // Strip the CIDR suffix.
                    let ip_only = ip_str.split('/').next().unwrap_or(ip_str);
                    if let Ok(ip) = ip_only.parse::<IpAddr>() {
                        return Ok(Some(ip));
                    }
                }
                return Ok(None);
            }
        }

        Ok(None)
    }

    /// Check if a sandbox container exists and get its status.
    pub async fn get_sandbox_phase(&self, name: &str) -> Result<SandboxPhase, tonic::Status> {
        let container_name = format!("{SANDBOX_PREFIX}{name}");
        let containers = self.list_containers_json().await?;

        for c in &containers {
            let id = c
                .pointer("/configuration/id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if id == container_name {
                let status = c
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("unknown");
                return Ok(match status {
                    "running" => SandboxPhase::Ready,
                    "stopped" => SandboxPhase::Unknown,
                    _ => SandboxPhase::Provisioning,
                });
            }
        }

        Err(tonic::Status::not_found(format!(
            "sandbox '{name}' not found"
        )))
    }

    // ── Helpers ──────────────────────────────────────────────────────

    async fn ensure_image(&self, image_ref: &str) -> Result<(), tonic::Status> {
        // Check if image exists locally.
        let output = tokio::process::Command::new("container")
            .args(["image", "list", "--format", "json"])
            .output()
            .await
            .map_err(|e| tonic::Status::internal(format!("failed to list images: {e}")))?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(images) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                for img in &images {
                    if let Some(reference) = img.get("reference").and_then(|r| r.as_str()) {
                        if reference == image_ref {
                            return Ok(());
                        }
                    }
                }
            }
        }

        // Pull the image.
        info!(image = %image_ref, "pulling sandbox image");
        let pull = tokio::process::Command::new("container")
            .args(["image", "pull", image_ref])
            .output()
            .await
            .map_err(|e| tonic::Status::internal(format!("failed to pull image: {e}")))?;

        if !pull.status.success() {
            let stderr = String::from_utf8_lossy(&pull.stderr);
            return Err(tonic::Status::internal(format!(
                "failed to pull image '{image_ref}': {stderr}"
            )));
        }

        Ok(())
    }

    async fn list_containers_json(&self) -> Result<Vec<serde_json::Value>, tonic::Status> {
        let output = tokio::process::Command::new("container")
            .args(["list", "--all", "--format", "json"])
            .output()
            .await
            .map_err(|e| tonic::Status::internal(format!("failed to list containers: {e}")))?;

        if !output.status.success() {
            return Ok(vec![]);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(&stdout).map_err(|e| {
            tonic::Status::internal(format!("failed to parse container list JSON: {e}"))
        })
    }

    fn sandbox_env_vars(&self, sandbox: &Sandbox, spec: &SandboxSpec) -> Vec<(String, String)> {
        let mut env = vec![
            (
                "OPENSHELL_GRPC_ENDPOINT".to_string(),
                self.grpc_endpoint.clone(),
            ),
            (
                "OPENSHELL_SSH_LISTEN_ADDR".to_string(),
                self.ssh_listen_addr.clone(),
            ),
            (
                "OPENSHELL_SSH_HANDSHAKE_SECRET".to_string(),
                self.ssh_handshake_secret.clone(),
            ),
            (
                "OPENSHELL_SSH_HANDSHAKE_SKEW_SECS".to_string(),
                self.ssh_handshake_skew_secs.to_string(),
            ),
            ("OPENSHELL_SANDBOX_ID".to_string(), sandbox.id.clone()),
            ("OPENSHELL_SANDBOX_NAME".to_string(), sandbox.name.clone()),
        ];

        // User-specified environment variables from the sandbox spec.
        for (key, value) in &spec.environment {
            env.push((key.clone(), value.clone()));
        }

        env
    }
}
