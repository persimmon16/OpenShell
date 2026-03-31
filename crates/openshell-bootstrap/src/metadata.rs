// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use crate::container_runtime::RuntimeType;
use crate::paths::{active_gateway_path, gateways_dir, last_sandbox_path};
use miette::{IntoDiagnostic, Result, WrapErr};
use openshell_core::paths::ensure_parent_dir_restricted;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Gateway metadata stored alongside deployment info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayMetadata {
    /// The gateway name.
    pub name: String,
    /// Gateway endpoint URL (e.g., `https://127.0.0.1:8080`).
    pub gateway_endpoint: String,
    /// Whether this is a remote gateway.
    pub is_remote: bool,
    /// Host port mapped to the gateway.
    pub gateway_port: u16,
    /// For remote gateways, the SSH destination (e.g., `user@hostname`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_host: Option<String>,
    /// For remote gateways, the resolved hostname/IP from SSH config.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resolved_host: Option<String>,

    /// Auth mode: `None` or `"mtls"` = mTLS (default), `"cloudflare_jwt"` = CF JWT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<String>,

    /// Edge proxy team/org domain (e.g., `brevlab.cloudflareaccess.com`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "cf_team_domain"
    )]
    pub edge_team_domain: Option<String>,

    /// URL for triggering re-authentication in the browser.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "cf_auth_url"
    )]
    pub edge_auth_url: Option<String>,

    /// The container runtime used for this gateway.
    /// Defaults to AppleContainer for this macOS-only fork.
    #[serde(default = "default_runtime_type")]
    pub runtime_type: RuntimeType,
}

fn default_runtime_type() -> RuntimeType {
    RuntimeType::AppleContainer
}

pub fn create_gateway_metadata(
    name: &str,
    port: u16,
) -> GatewayMetadata {
    create_gateway_metadata_with_host(name, port, None, false)
}

/// Create gateway metadata, optionally overriding the gateway host.
///
/// When `gateway_host` is `Some`, that value is used as the host portion of
/// `gateway_endpoint` instead of the default (`127.0.0.1`).
///
/// When `disable_tls` is `true`, the gateway endpoint uses the `http://`
/// scheme instead of `https://`.  This must match the server configuration
/// so that the CLI connects with the correct protocol.
pub fn create_gateway_metadata_with_host(
    name: &str,
    port: u16,
    gateway_host: Option<&str>,
    disable_tls: bool,
) -> GatewayMetadata {
    let scheme = if disable_tls { "http" } else { "https" };

    let host = gateway_host.map_or_else(
        || local_gateway_host().unwrap_or_else(|| "127.0.0.1".to_string()),
        String::from,
    );
    let gateway_endpoint = format!("{scheme}://{host}:{port}");

    GatewayMetadata {
        name: name.to_string(),
        gateway_endpoint,
        is_remote: false,
        gateway_port: port,
        remote_host: None,
        resolved_host: None,
        auth_mode: None,
        edge_team_domain: None,
        edge_auth_url: None,
        runtime_type: RuntimeType::AppleContainer,
    }
}

pub fn local_gateway_host() -> Option<String> {
    // No DOCKER_HOST lookup needed — always local macOS.
    None
}

fn stored_metadata_path(name: &str) -> Result<PathBuf> {
    Ok(gateways_dir()?.join(name).join("metadata.json"))
}

/// Extract the hostname from an SSH destination string.
///
/// Handles formats like:
/// - `user@hostname` -> `hostname`
/// - `ssh://user@hostname` -> `hostname`
/// - `hostname` -> `hostname`
pub fn extract_host_from_ssh_destination(destination: &str) -> String {
    let dest = destination.strip_prefix("ssh://").unwrap_or(destination);

    // Handle user@host format
    dest.find('@')
        .map_or_else(|| dest.to_string(), |at_pos| dest[at_pos + 1..].to_string())
}

/// Resolve an SSH host alias to the actual hostname or IP address.
///
/// Uses `ssh -G <host>` to query the effective SSH configuration, which
/// resolves `~/.ssh/config` aliases and `HostName` directives. Falls back
/// to the original host string if the command fails.
pub fn resolve_ssh_hostname(host: &str) -> String {
    let output = std::process::Command::new("ssh")
        .args(["-G", host])
        .output();

    match output {
        Ok(result) if result.status.success() => {
            let stdout = String::from_utf8_lossy(&result.stdout);
            for line in stdout.lines() {
                if let Some(value) = line.strip_prefix("hostname ") {
                    let resolved = value.trim();
                    if !resolved.is_empty() {
                        tracing::debug!(
                            ssh_host = host,
                            resolved_hostname = resolved,
                            "resolved SSH host alias"
                        );
                        return resolved.to_string();
                    }
                }
            }
            // ssh -G succeeded but no hostname line found; use original
            host.to_string()
        }
        Ok(result) => {
            tracing::warn!(
                ssh_host = host,
                stderr = %String::from_utf8_lossy(&result.stderr).trim(),
                "ssh -G failed, using original host"
            );
            host.to_string()
        }
        Err(err) => {
            tracing::warn!(
                ssh_host = host,
                error = %err,
                "failed to run ssh -G, using original host"
            );
            host.to_string()
        }
    }
}

pub fn store_gateway_metadata(name: &str, metadata: &GatewayMetadata) -> Result<()> {
    let path = stored_metadata_path(name)?;
    ensure_parent_dir_restricted(&path)?;
    let contents = serde_json::to_string_pretty(metadata)
        .into_diagnostic()
        .wrap_err("failed to serialize gateway metadata")?;
    std::fs::write(&path, contents)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write metadata to {}", path.display()))?;
    Ok(())
}

pub fn load_gateway_metadata(name: &str) -> Result<GatewayMetadata> {
    let path = stored_metadata_path(name)?;
    let contents = std::fs::read_to_string(&path)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read metadata from {}", path.display()))?;
    serde_json::from_str(&contents)
        .into_diagnostic()
        .wrap_err("failed to parse gateway metadata")
}

/// Load gateway metadata if available.
pub fn get_gateway_metadata(name: &str) -> Option<GatewayMetadata> {
    load_gateway_metadata(name).ok()
}

/// Save the active gateway name to persistent storage.
pub fn save_active_gateway(name: &str) -> Result<()> {
    let path = active_gateway_path()?;
    ensure_parent_dir_restricted(&path)?;
    std::fs::write(&path, name)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write active gateway to {}", path.display()))?;
    Ok(())
}

/// Load the active gateway name from persistent storage.
///
/// Returns `None` if no active gateway has been set.
pub fn load_active_gateway() -> Option<String> {
    let path = active_gateway_path().ok()?;
    let contents = std::fs::read_to_string(&path).ok()?;
    let name = contents.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// Save the last-used sandbox name for a gateway to persistent storage.
pub fn save_last_sandbox(gateway: &str, sandbox: &str) -> Result<()> {
    let path = last_sandbox_path(gateway)?;
    ensure_parent_dir_restricted(&path)?;
    std::fs::write(&path, sandbox)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write last sandbox to {}", path.display()))?;
    Ok(())
}

/// Load the last-used sandbox name for a gateway from persistent storage.
///
/// Returns `None` if no last sandbox has been set.
pub fn load_last_sandbox(gateway: &str) -> Option<String> {
    let path = last_sandbox_path(gateway).ok()?;
    let contents = std::fs::read_to_string(&path).ok()?;
    let name = contents.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// Clear the last-used sandbox record for a gateway if it matches the given name.
///
/// This should be called after a sandbox is deleted so that subsequent commands
/// don't try to connect to a sandbox that no longer exists.
pub fn clear_last_sandbox_if_matches(gateway: &str, sandbox: &str) {
    if let Some(current) = load_last_sandbox(gateway) {
        if current == sandbox {
            if let Ok(path) = last_sandbox_path(gateway) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// List all gateways that have stored metadata.
///
/// Scans `$XDG_CONFIG_HOME/openshell/gateways/` for subdirectories containing
/// `metadata.json` and returns the parsed metadata for each.
pub fn list_gateways() -> Result<Vec<GatewayMetadata>> {
    let dir = gateways_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut gateways = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read directory {}", dir.display()))?;

    for entry in entries {
        let entry = entry.into_diagnostic()?;
        let path = entry.path();
        // Only consider directories that contain a metadata.json file
        if path.is_dir() {
            let gateway_name = entry.file_name().to_string_lossy().to_string();
            if let Ok(metadata) = load_gateway_metadata(&gateway_name) {
                gateways.push(metadata);
            }
        }
    }

    // Sort by name for stable output
    gateways.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(gateways)
}

/// Remove the active gateway file (used when destroying the active gateway).
pub fn clear_active_gateway() -> Result<()> {
    let path = active_gateway_path()?;
    if path.exists() {
        std::fs::remove_file(&path)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

/// Remove gateway metadata file.
pub fn remove_gateway_metadata(name: &str) -> Result<()> {
    let path = stored_metadata_path(name)?;
    if path.exists() {
        std::fs::remove_file(&path)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_host_plain_hostname() {
        assert_eq!(extract_host_from_ssh_destination("myserver"), "myserver");
    }

    #[test]
    fn extract_host_user_at_hostname() {
        assert_eq!(
            extract_host_from_ssh_destination("ubuntu@myserver"),
            "myserver"
        );
    }

    #[test]
    fn extract_host_ssh_scheme() {
        assert_eq!(
            extract_host_from_ssh_destination("ssh://ubuntu@myserver"),
            "myserver"
        );
    }

    #[test]
    fn extract_host_ssh_scheme_no_user() {
        assert_eq!(
            extract_host_from_ssh_destination("ssh://myserver"),
            "myserver"
        );
    }

    #[test]
    fn local_gateway_metadata() {
        let meta = create_gateway_metadata("test", 8080);
        assert_eq!(meta.name, "test");
        assert_eq!(meta.gateway_endpoint, "https://127.0.0.1:8080");
        assert_eq!(meta.gateway_port, 8080);
        assert!(!meta.is_remote);
        assert!(meta.remote_host.is_none());
        assert!(meta.resolved_host.is_none());
    }

    #[test]
    fn local_gateway_metadata_custom_port() {
        let meta = create_gateway_metadata("test", 9090);
        assert_eq!(meta.gateway_endpoint, "https://127.0.0.1:9090");
        assert_eq!(meta.gateway_port, 9090);
    }

    #[test]
    fn metadata_roundtrip() {
        let meta = GatewayMetadata {
            name: "test".to_string(),
            gateway_endpoint: "https://127.0.0.1:8080".to_string(),
            is_remote: false,
            gateway_port: 8080,
            remote_host: None,
            resolved_host: None,
            auth_mode: None,
            edge_team_domain: None,
            edge_auth_url: None,
            runtime_type: RuntimeType::AppleContainer,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: GatewayMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.gateway_endpoint, "https://127.0.0.1:8080");
        assert_eq!(parsed.gateway_port, 8080);
    }

    #[test]
    fn metadata_deserialize_without_resolved_host() {
        // Existing metadata files won't have the resolved_host field.
        // Ensure backwards compatibility via serde(default).
        let json = r#"{
            "name": "test",
            "gateway_endpoint": "https://127.0.0.1:8080",
            "is_remote": false,
            "gateway_port": 8080
        }"#;
        let parsed: GatewayMetadata = serde_json::from_str(json).unwrap();
        assert!(parsed.resolved_host.is_none());
    }

    #[test]
    fn local_gateway_metadata_with_gateway_host_override() {
        let meta = create_gateway_metadata_with_host(
            "test",
            8080,
            Some("custom.host"),
            false,
        );
        assert_eq!(meta.name, "test");
        assert_eq!(meta.gateway_endpoint, "https://custom.host:8080");
        assert_eq!(meta.gateway_port, 8080);
        assert!(!meta.is_remote);
        assert!(meta.remote_host.is_none());
        assert!(meta.resolved_host.is_none());
    }

    #[test]
    fn local_gateway_metadata_with_no_gateway_host_override() {
        let meta = create_gateway_metadata_with_host("test", 8080, None, false);
        assert_eq!(meta.gateway_endpoint, "https://127.0.0.1:8080");
    }

    #[test]
    fn local_gateway_metadata_with_tls_disabled() {
        let meta = create_gateway_metadata_with_host("test", 8080, None, true);
        assert_eq!(meta.gateway_endpoint, "http://127.0.0.1:8080");
    }

    #[test]
    fn local_gateway_metadata_with_tls_disabled_and_gateway_host() {
        let meta = create_gateway_metadata_with_host(
            "test",
            8080,
            Some("custom.host"),
            true,
        );
        assert_eq!(meta.gateway_endpoint, "http://custom.host:8080");
    }

    // ── last-sandbox persistence ──────────────────────────────────────

    /// Helper: hold the shared XDG test lock, set `XDG_CONFIG_HOME` to a
    /// tempdir, run `f`, then restore the original value.
    #[allow(unsafe_code)]
    fn with_tmp_xdg<F: FnOnce()>(tmp: &std::path::Path, f: F) {
        let _guard = crate::XDG_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let orig = std::env::var("XDG_CONFIG_HOME").ok();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp);
        }
        f();
        unsafe {
            match orig {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    #[test]
    fn save_and_load_last_sandbox_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        with_tmp_xdg(tmp.path(), || {
            save_last_sandbox("mygateway", "dev-box").unwrap();
            assert_eq!(load_last_sandbox("mygateway"), Some("dev-box".to_string()));
        });
    }

    #[test]
    fn load_last_sandbox_returns_none_when_not_set() {
        let tmp = tempfile::tempdir().unwrap();
        with_tmp_xdg(tmp.path(), || {
            assert_eq!(load_last_sandbox("no-such-gateway"), None);
        });
    }

    #[test]
    fn save_last_sandbox_overwrites_previous() {
        let tmp = tempfile::tempdir().unwrap();
        with_tmp_xdg(tmp.path(), || {
            save_last_sandbox("g1", "first").unwrap();
            save_last_sandbox("g1", "second").unwrap();
            assert_eq!(load_last_sandbox("g1"), Some("second".to_string()));
        });
    }

    #[test]
    fn save_last_sandbox_creates_parent_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        with_tmp_xdg(tmp.path(), || {
            save_last_sandbox("brand-new-gateway", "sb1").unwrap();
            assert_eq!(
                load_last_sandbox("brand-new-gateway"),
                Some("sb1".to_string())
            );
        });
    }

    #[test]
    fn load_last_sandbox_ignores_whitespace() {
        let tmp = tempfile::tempdir().unwrap();
        with_tmp_xdg(tmp.path(), || {
            let path = last_sandbox_path("ws-gateway").unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "  my-sb \n").unwrap();
            assert_eq!(load_last_sandbox("ws-gateway"), Some("my-sb".to_string()));
        });
    }

    #[test]
    fn load_last_sandbox_returns_none_for_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        with_tmp_xdg(tmp.path(), || {
            let path = last_sandbox_path("empty-gateway").unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "   \n").unwrap();
            assert_eq!(load_last_sandbox("empty-gateway"), None);
        });
    }

    #[test]
    fn last_sandbox_is_per_gateway() {
        let tmp = tempfile::tempdir().unwrap();
        with_tmp_xdg(tmp.path(), || {
            save_last_sandbox("gateway-a", "sandbox-a").unwrap();
            save_last_sandbox("gateway-b", "sandbox-b").unwrap();
            assert_eq!(
                load_last_sandbox("gateway-a"),
                Some("sandbox-a".to_string())
            );
            assert_eq!(
                load_last_sandbox("gateway-b"),
                Some("sandbox-b".to_string())
            );
        });
    }
}
