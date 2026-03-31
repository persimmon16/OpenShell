// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Apple Container runtime backend (macOS only).
//!
//! Runs the gateway as a **native macOS process** instead of inside a
//! container. The gateway binary (`openshell-server`) is started as a
//! background daemon with its PID tracked in a file for lifecycle management.
//!
//! Sandboxes are Apple Container VMs managed directly by the gateway process
//! via the `container` CLI. No Kubernetes, no bridge daemon.

use crate::container_runtime::{
    ExistingGateway, GatewayContainerConfig, PortConflict, RuntimePreflight, RuntimeType,
};
use miette::{IntoDiagnostic, Result, WrapErr};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Apple Container runtime backend.
///
/// Manages the gateway as a native macOS process and uses Apple Container
/// for sandbox VMs.
pub struct AppleContainerRuntime;

impl AppleContainerRuntime {
    pub fn new() -> Self {
        Self
    }

    pub async fn check_available(&self) -> Result<RuntimePreflight> {
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

        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.contains("running") {
            return Err(miette::miette!(
                "Apple Container service is not running.\n\
                 Run `container system start` to start it."
            ));
        }

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
        stdout
            .strip_prefix("container CLI version ")
            .and_then(|s| s.split_whitespace().next())
            .map(String::from)
    }

    /// No-op for native gateway — no image needed.
    pub async fn ensure_image(
        &self,
        _image_ref: &str,
        _registry_username: Option<&str>,
        _registry_token: Option<&str>,
    ) -> Result<()> {
        Ok(())
    }

    /// No-op for native gateway.
    pub async fn pull_image(
        &self,
        _image_ref: &str,
        _registry_username: Option<&str>,
        _registry_token: Option<&str>,
        _on_progress: impl FnMut(String) + Send,
    ) -> Result<()> {
        Ok(())
    }

    /// Check if a gateway process is running by reading the PID file.
    pub async fn check_existing(&self, name: &str) -> Result<Option<ExistingGateway>> {
        let pid_path = self.pid_path(name);
        if !pid_path.exists() {
            return Ok(None);
        }

        let pid_str = std::fs::read_to_string(&pid_path).unwrap_or_default();
        let pid: i32 = match pid_str.trim().parse() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };

        // Check if process is alive.
        let alive = unsafe { libc::kill(pid, 0) } == 0;

        Ok(Some(ExistingGateway {
            container_exists: true,
            container_running: alive,
            storage_exists: self.data_dir(name).exists(),
            container_image: Some("native".to_string()),
        }))
    }

    /// Start the gateway as a native macOS background process.
    pub async fn create_gateway(&self, name: &str, config: &GatewayContainerConfig) -> Result<()> {
        let data_dir = self.data_dir(name);
        let pki_dir = self.pki_dir(name);
        let log_dir = self.log_dir(name);

        std::fs::create_dir_all(&data_dir).into_diagnostic()?;
        std::fs::create_dir_all(&pki_dir).into_diagnostic()?;
        std::fs::create_dir_all(&log_dir).into_diagnostic()?;

        let server_bin = find_server_binary()?;

        // Generate SSH handshake secret.
        let secret = generate_secret();

        // Build server arguments.
        let db_url = format!("sqlite://{}/openshell.db", data_dir.display());
        let sandbox_image = std::env::var("OPENSHELL_SANDBOX_IMAGE").unwrap_or_else(|_| {
            "ghcr.io/nvidia/openshell-community/sandboxes/base:latest".to_string()
        });
        let mut args = vec![
            "--port".to_string(),
            config.gateway_port.to_string(),
            "--sandbox-backend".to_string(),
            "apple-container".to_string(),
            "--db-url".to_string(),
            db_url,
            "--ssh-handshake-secret".to_string(),
            secret,
            "--sandbox-image".to_string(),
            sandbox_image,
        ];

        if config.disable_tls {
            args.push("--disable-tls".to_string());
        }
        if config.disable_gateway_auth {
            args.push("--disable-gateway-auth".to_string());
        }

        // Start the server as a background process.
        let log_path = log_dir.join("server.log");
        let log_file = std::fs::File::create(&log_path)
            .into_diagnostic()
            .wrap_err("failed to create server log file")?;
        let err_file = log_file
            .try_clone()
            .into_diagnostic()
            .wrap_err("failed to clone log file handle")?;

        let child = std::process::Command::new(&server_bin)
            .args(&args)
            .stdout(log_file)
            .stderr(err_file)
            .stdin(std::process::Stdio::null())
            .spawn()
            .into_diagnostic()
            .wrap_err_with(|| {
                format!(
                    "failed to start openshell-server at {}",
                    server_bin.display()
                )
            })?;

        // Write PID file for lifecycle management.
        let pid_path = self.pid_path(name);
        std::fs::write(&pid_path, child.id().to_string())
            .into_diagnostic()
            .wrap_err("failed to write PID file")?;

        Ok(())
    }

    /// No-op — create_gateway already starts the process.
    pub async fn start_gateway(&self, _name: &str) -> Result<()> {
        Ok(())
    }

    /// Stop the gateway process via SIGTERM.
    pub async fn stop_gateway(&self, name: &str) -> Result<()> {
        if let Some(pid) = self.read_pid(name) {
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            // Wait briefly for graceful shutdown.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        Ok(())
    }

    /// Stop the gateway and clean up all data.
    pub async fn destroy_resources(&self, name: &str) -> Result<()> {
        self.stop_gateway(name).await?;

        let _ = std::fs::remove_file(self.pid_path(name));
        let _ = std::fs::remove_dir_all(self.data_dir(name));
        let _ = std::fs::remove_dir_all(self.pki_dir(name));
        let _ = std::fs::remove_dir_all(self.log_dir(name));

        Ok(())
    }

    /// No-op — native process uses host networking.
    pub async fn ensure_network(&self, _name: &str) -> Result<()> {
        Ok(())
    }

    pub async fn ensure_storage(&self, name: &str) -> Result<()> {
        std::fs::create_dir_all(self.data_dir(name))
            .into_diagnostic()
            .wrap_err("failed to create data directory")?;
        Ok(())
    }

    /// Execute a command — not applicable for native process.
    pub async fn exec_capture(&self, _name: &str, _cmd: Vec<String>) -> Result<(String, i64)> {
        Err(miette::miette!(
            "exec is not supported for native gateway process"
        ))
    }

    /// Wait for the gateway to become ready via TCP probe.
    pub async fn wait_for_ready(
        &self,
        name: &str,
        mut on_log: impl FnMut(String) + Send,
    ) -> Result<()> {
        let metadata = crate::metadata::get_gateway_metadata(name)
            .unwrap_or_else(|| crate::metadata::create_gateway_metadata(name, 8080));

        let endpoint = &metadata.gateway_endpoint;
        let port = metadata.gateway_port;
        let attempts = 30; // 30 seconds at 1s intervals (native process starts fast)

        for attempt in 0..attempts {
            if attempt > 0 && attempt % 5 == 0 {
                on_log(format!(
                    "[progress] Waiting for gateway ({attempt}/{attempts})"
                ));

                // Check if process is still alive.
                if let Some(pid) = self.read_pid(name) {
                    if unsafe { libc::kill(pid, 0) } != 0 {
                        // Process died — read log for diagnostics.
                        let log_path = self.log_dir(name).join("server.log");
                        let log_tail = std::fs::read_to_string(&log_path)
                            .unwrap_or_default()
                            .lines()
                            .rev()
                            .take(10)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect::<Vec<_>>()
                            .join("\n");
                        return Err(miette::miette!(
                            "Gateway process exited unexpectedly.\n\nServer log:\n{log_tail}"
                        ));
                    }
                }
            }

            match tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await {
                Ok(_) => {
                    on_log("[progress] Gateway is ready".to_string());
                    return Ok(());
                }
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }

        Err(miette::miette!(
            "Timed out waiting for gateway at {endpoint}"
        ))
    }

    /// Read recent lines from the server log file.
    pub async fn fetch_recent_logs(&self, name: &str, n: usize) -> String {
        let log_path = self.log_dir(name).join("server.log");
        match std::fs::read_to_string(&log_path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = lines.len().saturating_sub(n);
                lines[start..].join("\n")
            }
            Err(_) => "server logs: not available".to_string(),
        }
    }

    pub async fn stream_logs<W: std::io::Write + Send>(
        &self,
        name: &str,
        _follow: bool,
        lines: Option<usize>,
        mut writer: W,
    ) -> Result<()> {
        let content = self.fetch_recent_logs(name, lines.unwrap_or(100)).await;
        writer
            .write_all(content.as_bytes())
            .into_diagnostic()
            .wrap_err("failed to write log output")?;
        Ok(())
    }

    /// No-op — native process doesn't need image pushing.
    pub async fn push_images(
        &self,
        _name: &str,
        _images: &[&str],
        _on_log: &mut (dyn FnMut(String) + Send),
    ) -> Result<()> {
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
        self.build_image(dockerfile, tag, context, args, on_log)
            .await
    }

    pub async fn check_container_running(&self, name: &str) -> Result<()> {
        if let Some(pid) = self.read_pid(name) {
            if unsafe { libc::kill(pid, 0) } == 0 {
                return Ok(());
            }
        }
        Err(miette::miette!("gateway process is not running"))
    }

    // ── Path helpers ────────────────────────────────────────────────

    fn gateway_dir(&self, name: &str) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(format!("{home}/.openshell/gateways/{name}"))
    }

    fn data_dir(&self, name: &str) -> PathBuf {
        self.gateway_dir(name).join("data")
    }

    fn pki_dir(&self, name: &str) -> PathBuf {
        self.gateway_dir(name).join("pki")
    }

    fn log_dir(&self, name: &str) -> PathBuf {
        self.gateway_dir(name).join("logs")
    }

    fn pid_path(&self, name: &str) -> PathBuf {
        self.gateway_dir(name).join("gateway.pid")
    }

    fn read_pid(&self, name: &str) -> Option<i32> {
        std::fs::read_to_string(self.pid_path(name))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }
}

/// Find the `openshell-server` binary.
///
/// Search order:
/// 1. `OPENSHELL_SERVER_BIN` env var
/// 2. Same directory as the current executable
/// 3. PATH
fn find_server_binary() -> Result<PathBuf> {
    // Check env var first.
    if let Ok(bin) = std::env::var("OPENSHELL_SERVER_BIN") {
        let path = PathBuf::from(&bin);
        if path.exists() {
            return Ok(path);
        }
        return Err(miette::miette!("OPENSHELL_SERVER_BIN={bin} does not exist"));
    }

    // Check next to current executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("openshell-server");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // Check PATH.
    if let Ok(output) = std::process::Command::new("which")
        .arg("openshell-server")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }

    Err(miette::miette!(
        "Could not find openshell-server binary.\n\
         Build it with: cargo build --release -p openshell-server\n\
         Or set OPENSHELL_SERVER_BIN to point to it."
    ))
}

/// Generate a random hex secret for SSH handshake HMAC.
fn generate_secret() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .unwrap_or_default();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
