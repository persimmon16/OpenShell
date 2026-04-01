// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

pub mod container_runtime;
pub mod edge_token;
pub mod errors;
pub mod image;

mod constants;
mod metadata;
mod mtls;
mod paths;
mod pki;
mod runtime_apple;

/// Shared lock for tests that mutate the process-global `XDG_CONFIG_HOME`
/// env var. All such tests in any module must hold this lock to avoid
/// concurrent clobbering.
#[cfg(test)]
pub(crate) static XDG_TEST_LOCK: Mutex<()> = Mutex::new(());

use miette::{IntoDiagnostic, Result};
use std::sync::{Arc, Mutex};

use crate::container_runtime::{GatewayContainerConfig, RuntimeBackend};
use crate::metadata::{
    create_gateway_metadata, create_gateway_metadata_with_host, local_gateway_host,
};
use crate::mtls::store_pki_bundle;
use crate::pki::generate_pki;

pub use crate::constants::container_name;
pub use crate::container_runtime::{ExistingGateway, PortConflict, RuntimePreflight, RuntimeType};
pub use crate::metadata::{
    GatewayMetadata, clear_active_gateway, clear_last_sandbox_if_matches,
    extract_host_from_ssh_destination, get_gateway_metadata, list_gateways, load_active_gateway,
    load_gateway_metadata, load_last_sandbox, remove_gateway_metadata, resolve_ssh_hostname,
    save_active_gateway, save_last_sandbox, store_gateway_metadata,
};

/// Create the appropriate container runtime backend for the current platform.
///
/// Always returns Apple Container runtime (macOS only).
pub async fn create_runtime() -> Result<RuntimeBackend> {
    use runtime_apple::AppleContainerRuntime;
    Ok(RuntimeBackend::AppleContainer(AppleContainerRuntime::new()))
}

/// Default host port for the gateway.
pub const DEFAULT_GATEWAY_PORT: u16 = 8080;

#[derive(Debug, Clone)]
pub struct DeployOptions {
    pub name: String,
    pub image_ref: Option<String>,
    /// Host port for the gateway. Defaults to 8080.
    pub port: u16,
    /// Disable TLS entirely — the server listens on plaintext HTTP.
    pub disable_tls: bool,
    /// Disable gateway authentication (mTLS client certificate requirement).
    /// Ignored when `disable_tls` is true.
    pub disable_gateway_auth: bool,
    /// When true, destroy any existing gateway resources before deploying.
    /// When false, an existing gateway is left as-is and deployment is
    /// skipped (the caller is responsible for prompting the user first).
    pub recreate: bool,
}

impl DeployOptions {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            image_ref: None,
            port: DEFAULT_GATEWAY_PORT,
            disable_tls: false,
            disable_gateway_auth: false,
            recreate: false,
        }
    }

    /// Set the host port for the gateway.
    #[must_use]
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Disable TLS entirely — the server listens on plaintext HTTP.
    #[must_use]
    pub fn with_disable_tls(mut self, disable: bool) -> Self {
        self.disable_tls = disable;
        self
    }

    /// Disable gateway authentication (mTLS client certificate requirement).
    #[must_use]
    pub fn with_disable_gateway_auth(mut self, disable: bool) -> Self {
        self.disable_gateway_auth = disable;
        self
    }

    /// Set whether to destroy and recreate existing gateway resources.
    #[must_use]
    pub fn with_recreate(mut self, recreate: bool) -> Self {
        self.recreate = recreate;
        self
    }
}

pub struct GatewayHandle {
    name: String,
    metadata: GatewayMetadata,
    runtime: RuntimeBackend,
}

impl GatewayHandle {
    /// Get the gateway metadata.
    pub fn metadata(&self) -> &GatewayMetadata {
        &self.metadata
    }

    /// Get the gateway endpoint URL.
    pub fn gateway_endpoint(&self) -> &str {
        &self.metadata.gateway_endpoint
    }

    /// Get a reference to the runtime backend.
    pub fn runtime(&self) -> &RuntimeBackend {
        &self.runtime
    }

    pub async fn stop(&self) -> Result<()> {
        self.runtime.stop_gateway(&self.name).await
    }

    pub async fn destroy(&self) -> Result<()> {
        self.runtime.destroy_resources(&self.name).await
    }
}

/// Check whether a gateway with the given name already has resources deployed.
///
/// Returns `None` if no existing gateway resources are found, or
/// `Some(ExistingGateway)` with details about what exists.
pub async fn check_existing_deployment(
    name: &str,
) -> Result<Option<ExistingGateway>> {
    let runtime = create_runtime().await?;
    runtime.check_existing(name).await
}

pub async fn deploy_gateway(options: DeployOptions) -> Result<GatewayHandle> {
    deploy_gateway_with_logs(options, |_| {}).await
}

pub async fn deploy_gateway_with_logs<F>(options: DeployOptions, on_log: F) -> Result<GatewayHandle>
where
    F: FnMut(String) + Send + 'static,
{
    let name = options.name;
    let port = options.port;
    let disable_tls = options.disable_tls;
    let disable_gateway_auth = options.disable_gateway_auth;
    let recreate = options.recreate;

    // Wrap on_log in Arc<Mutex<>> so we can share it.
    let on_log = Arc::new(Mutex::new(on_log));

    // Helper to call on_log from the shared reference
    let log = |msg: String| {
        if let Ok(mut f) = on_log.lock() {
            f(msg);
        }
    };

    // Select the container runtime for this deployment.
    log("[status] Checking runtime".to_string());
    let runtime = create_runtime().await?;

    // If an existing gateway is found, either tear it down (when recreate is
    // requested) or bail out so the caller can prompt the user / reuse it.
    if let Some(existing) = runtime.check_existing(&name).await? {
        if recreate {
            log("[status] Removing existing gateway".to_string());
            runtime.destroy_resources(&name).await?;
        } else {
            return Err(miette::miette!(
                "Gateway '{name}' already exists (container_running={}).\n\
                 Use --recreate to destroy and redeploy, or destroy it first with:\n\n    \
                 openshell gateway destroy --name {name}",
                existing.container_running,
            ));
        }
    }

    log("[status] Initializing environment".to_string());
    runtime.ensure_network(&name).await?;
    runtime.ensure_storage(&name).await?;

    // Compute extra TLS SANs.
    let extra_sans: Vec<String> = local_gateway_host().into_iter().collect();

    // Check for port conflicts before creating/starting the container.
    let conflicts = runtime.check_port_conflicts(&name, port).await?;
    if !conflicts.is_empty() {
        let details: Vec<String> = conflicts
            .iter()
            .map(|c| {
                format!(
                    "port {} is held by process \"{}\"",
                    c.host_port, c.container_name
                )
            })
            .collect();
        return Err(miette::miette!(
            "cannot start gateway: {}\n\nStop the conflicting process(es) first, \
             then retry.",
            details.join(", "),
        ));
    }

    // From this point on, runtime resources are being created. If any
    // subsequent step fails, clean up to avoid orphaned state.
    let config = GatewayContainerConfig {
        image_ref: "native".to_string(),
        extra_sans: extra_sans.clone(),
        ssh_gateway_host: None,
        gateway_port: port,
        disable_tls,
        disable_gateway_auth,
        registry_username: None,
        registry_token: None,
        gpu: false,
    };

    let deploy_result: Result<GatewayMetadata> = async {
        runtime.create_gateway(&name, &config).await?;
        runtime.start_gateway(&name).await?;

        // Apple Container path: generate PKI and write to
        // the volume-mounted directory so the gateway server picks them up.
        log("[progress] Generating TLS certificates".to_string());
        let pki_bundle = generate_pki(&extra_sans)?;
        store_pki_bundle(&name, &pki_bundle)?;

        // Write server-side PKI to the Apple Container volume mount.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let pki_dir = format!("{home}/.openshell/gateways/{name}/pki");
        std::fs::create_dir_all(&pki_dir).into_diagnostic()?;
        std::fs::write(format!("{pki_dir}/ca.crt"), &pki_bundle.ca_cert_pem)
            .into_diagnostic()?;
        std::fs::write(format!("{pki_dir}/tls.crt"), &pki_bundle.server_cert_pem)
            .into_diagnostic()?;
        std::fs::write(format!("{pki_dir}/tls.key"), &pki_bundle.server_key_pem)
            .into_diagnostic()?;

        log("[status] Starting gateway".to_string());
        {
            let on_log_ref = Arc::clone(&on_log);
            let mut gateway_log = move |msg: String| {
                if let Ok(mut f) = on_log_ref.lock() {
                    f(msg);
                }
            };
            runtime.wait_for_ready(&name, &mut gateway_log).await?;
        }

        // Create and store gateway metadata.
        let mut metadata = create_gateway_metadata_with_host(
            &name,
            port,
            None,
            disable_tls,
        );
        metadata.runtime_type = runtime.runtime_type();
        store_gateway_metadata(&name, &metadata)?;

        Ok(metadata)
    }
    .await;

    match deploy_result {
        Ok(metadata) => Ok(GatewayHandle {
            name,
            metadata,
            runtime,
        }),
        Err(deploy_err) => {
            tracing::info!("deploy failed, cleaning up gateway resources for '{name}'");
            if let Err(cleanup_err) = runtime.destroy_resources(&name).await {
                tracing::warn!(
                    "automatic cleanup after failed deploy also failed: {cleanup_err}. \
                     Manual cleanup may be required: \
                     openshell gateway destroy --name {name}"
                );
            }
            Err(deploy_err)
        }
    }
}

/// Get a handle to an existing gateway.
pub async fn gateway_handle(name: &str) -> Result<GatewayHandle> {
    let runtime = create_runtime().await?;
    let metadata = load_gateway_metadata(name)
        .unwrap_or_else(|_| create_gateway_metadata(name, DEFAULT_GATEWAY_PORT));
    Ok(GatewayHandle {
        name: name.to_string(),
        metadata,
        runtime,
    })
}

/// Fetch logs from the gateway container.
///
/// Uses the appropriate runtime backend based on gateway metadata.
///
/// When `follow` is true, streams logs in real-time (blocks until cancelled).
/// When `lines` is `Some(n)`, returns the last `n` lines; when `None`,
/// returns all available logs.
pub async fn gateway_container_logs<W: std::io::Write>(
    name: &str,
    lines: Option<usize>,
    follow: bool,
    mut writer: W,
) -> Result<()> {
    // Fetch all log output into a buffer and then write to the non-Send writer.
    // This avoids requiring Send on the writer while still using the runtime.
    let runtime = create_runtime().await?;
    let mut buf = Vec::new();
    runtime.stream_logs(name, follow, lines, &mut buf).await?;
    writer
        .write_all(&buf)
        .into_diagnostic()
        .map_err(|e| e.wrap_err("failed to write log output"))?;
    Ok(())
}

/// Fetch the last `n` lines of container logs for a local gateway as a
/// `String`.  This is a convenience wrapper for diagnostic call sites (e.g.
/// failure diagnosis in the CLI) that do not hold a runtime client handle.
///
/// Returns an empty string on any connection error so callers don't
/// need to worry about error handling.
pub async fn fetch_gateway_logs(name: &str, n: usize) -> String {
    let runtime = match create_runtime().await {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    runtime.fetch_recent_logs(name, n).await
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_existing_pki_bundle_validates_pem_markers() {
        // The PEM validation checks for "-----BEGIN " markers.
        // This test verifies that generate_pki produces bundles that
        // would pass that check.
        let bundle = generate_pki(&[]).expect("generate_pki failed");
        for (label, pem) in [
            ("ca_cert", &bundle.ca_cert_pem),
            ("server_cert", &bundle.server_cert_pem),
            ("server_key", &bundle.server_key_pem),
            ("client_cert", &bundle.client_cert_pem),
            ("client_key", &bundle.client_key_pem),
        ] {
            assert!(
                pem.contains("-----BEGIN "),
                "{label} should contain PEM marker"
            );
        }
    }
}
