// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Apple Container runtime backend (macOS only).
//!
//! This module implements the container runtime interface using the Apple
//! Container CLI (`container`), which manages lightweight VMs via the
//! Virtualization.framework. Unlike Docker, there is no k3s or Kubernetes
//! involved — sandboxes are managed directly via a Swift bridge daemon.

#![cfg(target_os = "macos")]

use crate::container_runtime::{
    ExistingGateway, GatewayContainerConfig, PortConflict, RuntimePreflight, RuntimeType,
};
use miette::{IntoDiagnostic, Result, WrapErr};
use std::collections::HashMap;
use std::path::Path;

/// Apple Container runtime backend.
///
/// Uses the `container` CLI to manage containers as lightweight VMs.
pub struct AppleContainerRuntime;

impl AppleContainerRuntime {
    pub fn new() -> Self {
        Self
    }

    pub async fn check_available(&self) -> Result<RuntimePreflight> {
        // Apple Container v0.10.0 uses `container system status` (not `system info`).
        let output = tokio::process::Command::new("container")
            .args(["system", "status"])
            .output()
            .await
            .into_diagnostic()
            .wrap_err("failed to run `container system status`")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(miette::miette!(
                "Apple Container is not running or not installed.\n\
                 Run `container system start` to start it.\n\n  {stderr}"
            ));
        }

        // Parse the table-format output. Look for "status  running".
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.contains("running") {
            return Err(miette::miette!(
                "Apple Container service is not running.\n\
                 Run `container system start` to start it."
            ));
        }

        // Get version from `container system version`.
        let version = self.get_version().await;

        Ok(RuntimePreflight {
            version,
            runtime_type: RuntimeType::AppleContainer,
        })
    }

    async fn get_version(&self) -> Option<String> {
        let output = tokio::process::Command::new("container")
            .args(["--version"])
            .output()
            .await
            .ok()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Output: "container CLI version 0.10.0 (build: release, commit: ...)"
        stdout
            .strip_prefix("container CLI version ")
            .and_then(|s| s.split_whitespace().next())
            .map(String::from)
    }

    pub async fn ensure_image(
        &self,
        image_ref: &str,
        _registry_username: Option<&str>,
        _registry_token: Option<&str>,
    ) -> Result<()> {
        // Check if image exists locally via JSON list.
        let check = tokio::process::Command::new("container")
            .args(["image", "list", "--format", "json"])
            .output()
            .await
            .into_diagnostic()?;

        if check.status.success() {
            let stdout = String::from_utf8_lossy(&check.stdout);
            // Parse JSON array and check `reference` field.
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
        let pull = tokio::process::Command::new("container")
            .args(["image", "pull", image_ref])
            .output()
            .await
            .into_diagnostic()
            .wrap_err("failed to pull image via Apple Container")?;

        if !pull.status.success() {
            let stderr = String::from_utf8_lossy(&pull.stderr);
            return Err(miette::miette!(
                "Failed to pull image '{image_ref}': {stderr}"
            ));
        }

        Ok(())
    }

    pub async fn pull_image(
        &self,
        image_ref: &str,
        registry_username: Option<&str>,
        registry_token: Option<&str>,
        mut on_progress: impl FnMut(String) + Send,
    ) -> Result<()> {
        on_progress(format!("[progress] Pulling {image_ref}"));
        self.ensure_image(image_ref, registry_username, registry_token)
            .await?;
        on_progress(format!("[progress] Pulled {image_ref}"));
        Ok(())
    }

    pub async fn check_existing(&self, name: &str) -> Result<Option<ExistingGateway>> {
        let container_name = format!("openshell-gateway-{name}");

        // Apple Container v0.10.0: `container list --all --format json`
        // Returns JSON array where each entry has:
        //   configuration.id  — the container name (set via --name at create time)
        //   status            — "running", "stopped", etc.
        //   configuration.image.reference — the image ref
        let output = tokio::process::Command::new("container")
            .args(["list", "--all", "--format", "json"])
            .output()
            .await
            .into_diagnostic()?;

        if !output.status.success() {
            return Ok(None);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let containers: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap_or_default();

        for c in &containers {
            let id = c.pointer("/configuration/id").and_then(|v| v.as_str());
            if id == Some(&container_name) {
                let running = c
                    .get("status")
                    .and_then(|s| s.as_str())
                    .is_some_and(|s| s == "running");
                let image = c
                    .pointer("/configuration/image/reference")
                    .and_then(|i| i.as_str())
                    .map(String::from);
                return Ok(Some(ExistingGateway {
                    container_exists: true,
                    container_running: running,
                    storage_exists: true,
                    container_image: image,
                }));
            }
        }

        Ok(None)
    }

    pub async fn create_gateway(&self, name: &str, config: &GatewayContainerConfig) -> Result<()> {
        let container_name = format!("openshell-gateway-{name}");
        let data_dir = self.data_dir(name);
        let pki_dir = self.pki_dir(name);

        // Ensure host directories exist.
        std::fs::create_dir_all(&data_dir).into_diagnostic()?;
        std::fs::create_dir_all(&pki_dir).into_diagnostic()?;

        let mut args = vec![
            "create".to_string(),
            "--name".to_string(),
            container_name,
            "-d".to_string(), // detach
            "-p".to_string(),
            // Gateway-only image listens on port 8080 directly (no k3s NodePort).
            format!("127.0.0.1:{}:8080", config.gateway_port),
            "-v".to_string(),
            format!("{pki_dir}:/etc/openshell-tls"),
            "-v".to_string(),
            format!("{data_dir}:/var/openshell"),
        ];

        // Environment variables.
        let env_vars = self.build_env_vars(config);
        for var in &env_vars {
            args.push("-e".to_string());
            args.push(var.clone());
        }

        args.push(config.image_ref.clone());

        let output = tokio::process::Command::new("container")
            .args(&args)
            .output()
            .await
            .into_diagnostic()
            .wrap_err("failed to create Apple Container gateway")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(miette::miette!(
                "Failed to create gateway container: {stderr}"
            ));
        }

        Ok(())
    }

    pub async fn start_gateway(&self, name: &str) -> Result<()> {
        let container_name = format!("openshell-gateway-{name}");
        let output = tokio::process::Command::new("container")
            .args(["start", &container_name])
            .output()
            .await
            .into_diagnostic()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(miette::miette!(
                "Failed to start gateway container: {stderr}"
            ));
        }
        Ok(())
    }

    pub async fn stop_gateway(&self, name: &str) -> Result<()> {
        let container_name = format!("openshell-gateway-{name}");
        let _ = tokio::process::Command::new("container")
            .args(["stop", &container_name])
            .output()
            .await;
        Ok(())
    }

    pub async fn destroy_resources(&self, name: &str) -> Result<()> {
        let container_name = format!("openshell-gateway-{name}");

        // Stop and remove the container.
        let _ = tokio::process::Command::new("container")
            .args(["stop", &container_name])
            .output()
            .await;
        let _ = tokio::process::Command::new("container")
            .args(["delete", &container_name])
            .output()
            .await;

        // Clean up host data directories.
        let data_dir = self.data_dir(name);
        let pki_dir = self.pki_dir(name);
        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_dir_all(&pki_dir);

        Ok(())
    }

    pub async fn ensure_network(&self, _name: &str) -> Result<()> {
        // Apple Container uses vmnet framework — no explicit network creation needed.
        Ok(())
    }

    pub async fn ensure_storage(&self, name: &str) -> Result<()> {
        let data_dir = self.data_dir(name);
        std::fs::create_dir_all(&data_dir)
            .into_diagnostic()
            .wrap_err("failed to create data directory")?;
        Ok(())
    }

    pub async fn exec_capture(&self, name: &str, cmd: Vec<String>) -> Result<(String, i64)> {
        let container_name = format!("openshell-gateway-{name}");
        let mut args = vec!["exec".to_string(), container_name];
        args.extend(cmd);

        let output = tokio::process::Command::new("container")
            .args(&args)
            .output()
            .await
            .into_diagnostic()?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = if stderr.is_empty() {
            stdout
        } else {
            format!("{stdout}{stderr}")
        };

        let exit_code = output.status.code().map_or(1, |c| c as i64);
        Ok((combined, exit_code))
    }

    pub async fn wait_for_ready(
        &self,
        name: &str,
        mut on_log: impl FnMut(String) + Send,
    ) -> Result<()> {
        let metadata = crate::metadata::get_gateway_metadata(name)
            .unwrap_or_else(|| crate::metadata::create_gateway_metadata(name, None, 8080));

        let endpoint = &metadata.gateway_endpoint;
        let attempts = 90; // 3 minutes at 2s intervals

        for attempt in 0..attempts {
            if attempt > 0 && attempt % 10 == 0 {
                on_log(format!(
                    "[progress] Waiting for gateway to become ready ({attempt}/{attempts})"
                ));
            }

            let port = metadata.gateway_port;
            match tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await {
                Ok(_) => {
                    on_log("[progress] Gateway is ready".to_string());
                    return Ok(());
                }
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }

        Err(miette::miette!(
            "Timed out waiting for gateway to become ready at {endpoint}"
        ))
    }

    pub async fn fetch_recent_logs(&self, name: &str, n: usize) -> String {
        let container_name = format!("openshell-gateway-{name}");
        // Apple Container v0.10.0 uses `-n` not `--tail`.
        let output = tokio::process::Command::new("container")
            .args(["logs", "-n", &n.to_string(), &container_name])
            .output()
            .await;

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                if stdout.is_empty() && stderr.is_empty() {
                    "container logs: none available".to_string()
                } else {
                    format!("container logs:\n  {stdout}{stderr}")
                }
            }
            Err(_) => "container logs: failed to fetch".to_string(),
        }
    }

    pub async fn stream_logs<W: std::io::Write + Send>(
        &self,
        name: &str,
        follow: bool,
        lines: Option<usize>,
        mut writer: W,
    ) -> Result<()> {
        let container_name = format!("openshell-gateway-{name}");
        let mut args = vec!["logs".to_string()];
        if follow {
            args.push("-f".to_string());
        }
        if let Some(n) = lines {
            // Apple Container v0.10.0 uses `-n` not `--tail`.
            args.push("-n".to_string());
            args.push(n.to_string());
        }
        args.push(container_name);

        let output = tokio::process::Command::new("container")
            .args(&args)
            .output()
            .await
            .into_diagnostic()?;

        writer
            .write_all(&output.stdout)
            .into_diagnostic()
            .wrap_err("failed to write log output")?;
        writer
            .write_all(&output.stderr)
            .into_diagnostic()
            .wrap_err("failed to write log output")?;

        Ok(())
    }

    pub async fn push_images(
        &self,
        _name: &str,
        _images: &[&str],
        _on_log: &mut (dyn FnMut(String) + Send),
    ) -> Result<()> {
        // No-op on macOS: images are available locally on the host and the
        // bridge daemon creates sandbox containers from them directly.
        Ok(())
    }

    pub async fn check_port_conflicts(&self, _name: &str, port: u16) -> Result<Vec<PortConflict>> {
        let output = tokio::process::Command::new("lsof")
            .args(["-i", &format!(":{port}"), "-sTCP:LISTEN", "-t"])
            .output()
            .await
            .into_diagnostic()?;

        if output.status.success() && !output.stdout.is_empty() {
            Ok(vec![PortConflict {
                container_name: "unknown (port in use)".to_string(),
                host_port: port,
            }])
        } else {
            Ok(vec![])
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
        on_log(format!(
            "Building image {tag} from {}",
            dockerfile.display()
        ));

        let mut cmd_args = vec![
            "build".to_string(),
            "-t".to_string(),
            tag.to_string(),
            "-f".to_string(),
            dockerfile.to_string_lossy().to_string(),
        ];

        for (key, value) in args {
            cmd_args.push("--build-arg".to_string());
            cmd_args.push(format!("{key}={value}"));
        }

        cmd_args.push(context.to_string_lossy().to_string());

        let output = tokio::process::Command::new("container")
            .args(&cmd_args)
            .output()
            .await
            .into_diagnostic()
            .wrap_err("failed to build image via Apple Container")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(miette::miette!("Image build failed: {stderr}"));
        }

        on_log(format!("Built image {tag}"));
        Ok(())
    }

    pub async fn build_and_push_image(
        &self,
        dockerfile: &Path,
        tag: &str,
        context: &Path,
        _gateway_name: &str,
        args: &HashMap<String, String>,
        on_log: &mut (dyn FnMut(String) + Send),
    ) -> Result<()> {
        // On Apple Container, build locally — images are shared via the host.
        self.build_image(dockerfile, tag, context, args, on_log)
            .await
    }

    pub async fn check_container_running(&self, name: &str) -> Result<()> {
        let existing = self.check_existing(name).await?;
        match existing {
            Some(info) if info.container_running => Ok(()),
            Some(_) => Err(miette::miette!("gateway container is not running")),
            None => Err(miette::miette!("gateway container does not exist")),
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────

    fn data_dir(&self, name: &str) -> String {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        format!("{home}/.openshell/gateways/{name}/data")
    }

    fn pki_dir(&self, name: &str) -> String {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        format!("{home}/.openshell/gateways/{name}/pki")
    }

    fn build_env_vars(&self, config: &GatewayContainerConfig) -> Vec<String> {
        // The gateway-only image runs openshell-server directly with OPENSHELL_* env vars.
        // Generate a random handshake secret for the SSH tunnel HMAC.
        use std::io::Read;
        let mut bytes = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .unwrap_or_default();
        let secret: String = bytes.iter().map(|b| format!("{b:02x}")).collect();

        let mut env = vec![
            "OPENSHELL_SANDBOX_BACKEND=apple-container".to_string(),
            "OPENSHELL_DB_URL=sqlite:///var/openshell/openshell.db".to_string(),
            format!("OPENSHELL_SSH_HANDSHAKE_SECRET={secret}"),
        ];

        if config.disable_tls {
            env.push("OPENSHELL_DISABLE_TLS=true".to_string());
        }
        if config.disable_gateway_auth {
            env.push("OPENSHELL_DISABLE_GATEWAY_AUTH=true".to_string());
        }
        if let Some(ref host) = config.ssh_gateway_host {
            env.push(format!("OPENSHELL_SSH_GATEWAY_HOST={host}"));
            env.push(format!(
                "OPENSHELL_SSH_GATEWAY_PORT={}",
                config.gateway_port
            ));
        }

        env
    }
}
