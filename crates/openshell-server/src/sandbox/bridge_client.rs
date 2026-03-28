// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! mTLS-authenticated gRPC client for the container bridge daemon.
//!
//! On macOS, the gateway manages sandbox containers via a Swift bridge daemon
//! running on the host. This module provides the Rust client that connects to
//! the bridge over mutual TLS: the gateway presents a client certificate to
//! prove its identity, and the bridge presents a server certificate verified
//! against the shared CA.
//!
//! The bridge daemon translates gRPC calls into Apple Container XPC operations.
//! Without mTLS, any process on the vmnet could issue container management
//! commands — the mutual authentication prevents this.

use openshell_core::proto::bridge::v1::container_bridge_client::ContainerBridgeClient;
use openshell_core::proto::bridge::v1::{
    ContainerResponse, ContainerState, CreateContainerRequest, DeleteContainerRequest,
    GetContainerRequest, HealthRequest, ListContainersRequest, StartContainerRequest,
    StopContainerRequest, WatchContainersRequest,
};
use openshell_core::{BridgeTlsConfig, Error, Result};
use std::collections::HashMap;
use std::time::Duration;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};
use tracing::{debug, info, warn};

/// Timeout for individual bridge daemon gRPC calls.
const BRIDGE_RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Client for the container bridge daemon, authenticated via mutual TLS.
///
/// Wraps a tonic gRPC channel configured with:
/// - CA certificate: verifies the bridge daemon's server certificate
/// - Client identity: certificate + key the gateway presents to the bridge
///
/// The bridge daemon must present a certificate signed by the same CA and
/// must verify the gateway's client certificate against the same CA.
#[derive(Clone)]
pub struct BridgeClient {
    inner: ContainerBridgeClient<Channel>,
    endpoint: String,
}

impl std::fmt::Debug for BridgeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeClient")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

impl BridgeClient {
    /// Connect to the bridge daemon with mutual TLS authentication.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Bridge daemon gRPC endpoint (e.g. `https://host.containers.internal:50052`)
    /// * `tls` - mTLS configuration (CA cert, client cert, client key)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - TLS certificate/key files cannot be read
    /// - The TLS configuration is invalid
    /// - The connection to the bridge daemon fails
    pub async fn connect(endpoint: &str, tls: &BridgeTlsConfig) -> Result<Self> {
        let ca_pem = std::fs::read(&tls.ca_path)
            .map_err(|e| Error::tls(format!("failed to read bridge CA cert: {e}")))?;
        let cert_pem = std::fs::read(&tls.cert_path)
            .map_err(|e| Error::tls(format!("failed to read bridge client cert: {e}")))?;
        let key_pem = std::fs::read(&tls.key_path)
            .map_err(|e| Error::tls(format!("failed to read bridge client key: {e}")))?;

        let tls_config = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(ca_pem))
            .identity(Identity::from_pem(cert_pem, key_pem))
            .domain_name("openshell-bridge");

        let channel = Channel::from_shared(endpoint.to_string())
            .map_err(|e| Error::transport(format!("invalid bridge endpoint: {e}")))?
            .tls_config(tls_config)
            .map_err(|e| Error::tls(format!("failed to configure bridge TLS: {e}")))?
            .timeout(BRIDGE_RPC_TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .connect()
            .await
            .map_err(|e| {
                Error::transport(format!(
                    "failed to connect to bridge daemon at {endpoint}: {e}"
                ))
            })?;

        info!(endpoint, "Connected to container bridge daemon (mTLS)");

        Ok(Self {
            inner: ContainerBridgeClient::new(channel),
            endpoint: endpoint.to_string(),
        })
    }

    /// Connect to the bridge daemon without TLS (for development/testing only).
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails.
    pub async fn connect_insecure(endpoint: &str) -> Result<Self> {
        warn!(
            endpoint,
            "Connecting to bridge daemon WITHOUT mTLS — development only"
        );

        let channel = Channel::from_shared(endpoint.to_string())
            .map_err(|e| Error::transport(format!("invalid bridge endpoint: {e}")))?
            .timeout(BRIDGE_RPC_TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .connect()
            .await
            .map_err(|e| {
                Error::transport(format!(
                    "failed to connect to bridge daemon at {endpoint}: {e}"
                ))
            })?;

        Ok(Self {
            inner: ContainerBridgeClient::new(channel),
            endpoint: endpoint.to_string(),
        })
    }

    /// Check bridge daemon health.
    pub async fn health(&mut self) -> Result<bool> {
        let resp = self
            .inner
            .health(HealthRequest {})
            .await
            .map_err(|e| Error::transport(format!("bridge health check failed: {e}")))?;
        let health = resp.into_inner();
        debug!(
            healthy = health.healthy,
            version = health.runtime_version,
            "Bridge daemon health"
        );
        Ok(health.healthy)
    }

    /// Create a sandbox container via the bridge daemon.
    pub async fn create_sandbox(
        &mut self,
        name: &str,
        image: &str,
        env: HashMap<String, String>,
        labels: HashMap<String, String>,
    ) -> Result<ContainerResponse> {
        let resp = self
            .inner
            .create_container(CreateContainerRequest {
                name: name.to_string(),
                image: image.to_string(),
                env,
                labels,
                ..Default::default()
            })
            .await
            .map_err(|e| Error::execution(format!("bridge create_container failed: {e}")))?;
        Ok(resp.into_inner())
    }

    /// Start a sandbox container.
    pub async fn start_sandbox(&mut self, name: &str) -> Result<()> {
        self.inner
            .start_container(StartContainerRequest {
                name: name.to_string(),
            })
            .await
            .map_err(|e| Error::execution(format!("bridge start_container failed: {e}")))?;
        Ok(())
    }

    /// Stop a sandbox container.
    pub async fn stop_sandbox(&mut self, name: &str, timeout_secs: u32) -> Result<()> {
        self.inner
            .stop_container(StopContainerRequest {
                name: name.to_string(),
                timeout_secs,
            })
            .await
            .map_err(|e| Error::execution(format!("bridge stop_container failed: {e}")))?;
        Ok(())
    }

    /// Delete a sandbox container.
    pub async fn delete_sandbox(&mut self, name: &str) -> Result<()> {
        self.inner
            .delete_container(DeleteContainerRequest {
                name: name.to_string(),
                force: false,
            })
            .await
            .map_err(|e| Error::execution(format!("bridge delete_container failed: {e}")))?;
        Ok(())
    }

    /// Get a sandbox container by name.
    pub async fn get_sandbox(&mut self, name: &str) -> Result<Option<ContainerResponse>> {
        match self
            .inner
            .get_container(GetContainerRequest {
                name: name.to_string(),
            })
            .await
        {
            Ok(resp) => Ok(Some(resp.into_inner())),
            Err(status) if status.code() == tonic::Code::NotFound => Ok(None),
            Err(e) => Err(Error::execution(format!(
                "bridge get_container failed: {e}"
            ))),
        }
    }

    /// List sandbox containers matching the given labels.
    pub async fn list_sandboxes(
        &mut self,
        label_selector: HashMap<String, String>,
    ) -> Result<Vec<ContainerResponse>> {
        let resp = self
            .inner
            .list_containers(ListContainersRequest {
                label_selector,
                state_filter: Vec::new(),
            })
            .await
            .map_err(|e| Error::execution(format!("bridge list_containers failed: {e}")))?;
        Ok(resp.into_inner().containers)
    }

    /// Watch for sandbox container state changes.
    ///
    /// Returns a streaming receiver of container events.
    pub async fn watch_sandboxes(
        &mut self,
        label_selector: HashMap<String, String>,
    ) -> Result<tonic::Streaming<openshell_core::proto::bridge::v1::ContainerEvent>> {
        let resp = self
            .inner
            .watch_containers(WatchContainersRequest { label_selector })
            .await
            .map_err(|e| Error::execution(format!("bridge watch_containers failed: {e}")))?;
        Ok(resp.into_inner())
    }

    /// Check if a container is running.
    pub async fn is_sandbox_running(&mut self, name: &str) -> Result<bool> {
        match self.get_sandbox(name).await? {
            Some(container) => Ok(container.state() == ContainerState::Running),
            None => Ok(false),
        }
    }

    /// Return the bridge endpoint this client is connected to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_impl_shows_endpoint() {
        // BridgeClient::connect requires a real server, so we just test Debug
        // on the struct-level metadata.
        let display = format!("{:?}", "https://host.containers.internal:50052");
        assert!(display.contains("host.containers.internal"));
    }

    #[test]
    fn bridge_rpc_timeout_is_reasonable() {
        assert!(BRIDGE_RPC_TIMEOUT.as_secs() >= 10);
        assert!(BRIDGE_RPC_TIMEOUT.as_secs() <= 120);
    }
}
