// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Apple Container sandbox backend.
//!
//! Uses the container bridge gRPC service (running on the macOS host) to
//! manage sandbox containers instead of Kubernetes pods.

#![cfg(target_os = "macos")]

use openshell_core::proto::bridge::{
    container_bridge_client::ContainerBridgeClient, CreateContainerRequest, DeleteContainerRequest,
    GetContainerRequest, ListContainersRequest, PortMapping, StartContainerRequest,
    StopContainerRequest, VolumeMount,
};
use openshell_core::proto::{
    Sandbox, SandboxCondition, SandboxPhase, SandboxSpec, SandboxStatus, SandboxTemplate,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tonic::transport::Channel;
use tracing::{debug, info, warn};

const SANDBOX_MANAGED_LABEL: &str = "openshell.ai/managed-by";
const SANDBOX_MANAGED_VALUE: &str = "openshell";
const SANDBOX_ID_LABEL: &str = "openshell.ai/sandbox-id";

/// Sandbox client using the Apple Container bridge daemon.
#[derive(Clone)]
pub struct AppleContainerSandboxClient {
    bridge: ContainerBridgeClient<Channel>,
    default_image: String,
    grpc_endpoint: String,
    ssh_listen_addr: String,
    ssh_handshake_secret: String,
    ssh_handshake_skew_secs: u64,
}

impl std::fmt::Debug for AppleContainerSandboxClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppleContainerSandboxClient")
            .field("default_image", &self.default_image)
            .field("grpc_endpoint", &self.grpc_endpoint)
            .finish()
    }
}

impl AppleContainerSandboxClient {
    /// Connect to the bridge daemon at the given endpoint.
    pub async fn new(
        bridge_endpoint: String,
        default_image: String,
        grpc_endpoint: String,
        ssh_listen_addr: String,
        ssh_handshake_secret: String,
        ssh_handshake_skew_secs: u64,
    ) -> Result<Self, tonic::transport::Error> {
        let channel = Channel::from_shared(format!("https://{bridge_endpoint}"))
            .expect("valid bridge endpoint")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .connect()
            .await?;

        Ok(Self {
            bridge: ContainerBridgeClient::new(channel),
            default_image,
            grpc_endpoint,
            ssh_listen_addr,
            ssh_handshake_secret,
            ssh_handshake_skew_secs,
        })
    }

    /// Create a sandbox container via the bridge.
    pub async fn create_sandbox(
        &self,
        sandbox: &Sandbox,
    ) -> Result<(), tonic::Status> {
        let spec = sandbox.spec.as_ref().ok_or_else(|| {
            tonic::Status::invalid_argument("sandbox spec is required")
        })?;

        let image = if spec.image.is_empty() {
            self.default_image.clone()
        } else {
            spec.image.clone()
        };

        let name = &sandbox.name;
        let mut env = HashMap::new();

        // Core sandbox environment.
        env.insert("OPENSHELL_GRPC_ENDPOINT".to_string(), self.grpc_endpoint.clone());
        env.insert("OPENSHELL_SSH_LISTEN_ADDR".to_string(), self.ssh_listen_addr.clone());
        env.insert("OPENSHELL_SSH_HANDSHAKE_SECRET".to_string(), self.ssh_handshake_secret.clone());
        env.insert(
            "OPENSHELL_SSH_HANDSHAKE_SKEW_SECS".to_string(),
            self.ssh_handshake_skew_secs.to_string(),
        );

        // User-specified environment variables from the sandbox spec.
        for kv in &spec.env {
            env.insert(kv.name.clone(), kv.value.clone());
        }

        let mut labels = HashMap::new();
        labels.insert(SANDBOX_MANAGED_LABEL.to_string(), SANDBOX_MANAGED_VALUE.to_string());
        labels.insert(SANDBOX_ID_LABEL.to_string(), sandbox.id.clone());

        let request = CreateContainerRequest {
            name: name.clone(),
            image,
            env,
            ports: vec![],
            volumes: vec![],
            resources: None,
            command: vec![],
            labels,
        };

        let mut client = self.bridge.clone();
        client.create_container(request).await?;
        info!(sandbox = %name, "created sandbox container via bridge");

        // Start the container.
        let start_request = StartContainerRequest {
            name: name.clone(),
        };
        client.start_container(start_request).await?;
        info!(sandbox = %name, "started sandbox container via bridge");

        Ok(())
    }

    /// Delete a sandbox container via the bridge.
    pub async fn delete_sandbox(&self, name: &str) -> Result<(), tonic::Status> {
        let mut client = self.bridge.clone();

        // Stop first, then delete.
        let _ = client
            .stop_container(StopContainerRequest {
                name: name.to_string(),
                timeout_seconds: 10,
            })
            .await;

        client
            .delete_container(DeleteContainerRequest {
                name: name.to_string(),
                force: true,
            })
            .await?;

        info!(sandbox = %name, "deleted sandbox container via bridge");
        Ok(())
    }

    /// Get a sandbox container status via the bridge.
    pub async fn get_sandbox(&self, name: &str) -> Result<Option<Sandbox>, tonic::Status> {
        let mut client = self.bridge.clone();
        let response = client
            .get_container(GetContainerRequest {
                name: name.to_string(),
            })
            .await;

        match response {
            Ok(resp) => {
                let container = resp.into_inner();
                Ok(Some(bridge_container_to_sandbox(&container)))
            }
            Err(status) if status.code() == tonic::Code::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// List all sandbox containers managed by openshell.
    pub async fn list_sandboxes(&self) -> Result<Vec<Sandbox>, tonic::Status> {
        let mut client = self.bridge.clone();
        let mut labels = HashMap::new();
        labels.insert(SANDBOX_MANAGED_LABEL.to_string(), SANDBOX_MANAGED_VALUE.to_string());

        let response = client
            .list_containers(ListContainersRequest {
                label_selector: labels,
                all: true,
            })
            .await?;

        Ok(response
            .into_inner()
            .containers
            .iter()
            .map(bridge_container_to_sandbox)
            .collect())
    }
}

/// Convert a bridge ContainerResponse to an OpenShell Sandbox proto.
fn bridge_container_to_sandbox(
    container: &openshell_core::proto::bridge::ContainerResponse,
) -> Sandbox {
    use openshell_core::proto::bridge::ContainerState;

    let phase = match ContainerState::try_from(container.state).unwrap_or(ContainerState::Unknown) {
        ContainerState::Running => SandboxPhase::Running,
        ContainerState::Created => SandboxPhase::Pending,
        ContainerState::Stopped | ContainerState::Exited => SandboxPhase::Stopped,
        ContainerState::Unknown => SandboxPhase::Unknown,
    };

    let id = container
        .labels
        .get(SANDBOX_ID_LABEL)
        .cloned()
        .unwrap_or_default();

    Sandbox {
        id,
        name: container.name.clone(),
        spec: Some(SandboxSpec {
            image: container.image.clone(),
            ..Default::default()
        }),
        status: Some(SandboxStatus {
            phase: phase.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}
