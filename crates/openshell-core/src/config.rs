// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Configuration management for OpenShell components.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

/// Server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Address to bind the server to.
    #[serde(default = "default_bind_address")]
    pub bind_address: SocketAddr,

    /// Log level (trace, debug, info, warn, error).
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// TLS configuration.  When `None`, the server listens on plaintext HTTP.
    pub tls: Option<TlsConfig>,

    /// Database URL for persistence.
    pub database_url: String,

    /// Kubernetes namespace for sandboxes.
    #[serde(default = "default_sandbox_namespace")]
    pub sandbox_namespace: String,

    /// Default container image for sandboxes.
    #[serde(default)]
    pub sandbox_image: String,

    /// Kubernetes `imagePullPolicy` for sandbox pods (e.g. `Always`,
    /// `IfNotPresent`, `Never`).  Defaults to empty, which lets Kubernetes
    /// apply its own default (`:latest` → `Always`, anything else →
    /// `IfNotPresent`).
    #[serde(default)]
    pub sandbox_image_pull_policy: String,

    /// gRPC endpoint for sandboxes to connect back to OpenShell.
    /// Used by sandbox pods to fetch their policy at startup.
    #[serde(default)]
    pub grpc_endpoint: String,

    /// Public gateway host for SSH proxy connections.
    #[serde(default = "default_ssh_gateway_host")]
    pub ssh_gateway_host: String,

    /// Public gateway port for SSH proxy connections.
    #[serde(default = "default_ssh_gateway_port")]
    pub ssh_gateway_port: u16,

    /// Path for SSH CONNECT/upgrade requests.
    #[serde(default = "default_ssh_connect_path")]
    pub ssh_connect_path: String,

    /// SSH listen port inside sandbox pods.
    #[serde(default = "default_sandbox_ssh_port")]
    pub sandbox_ssh_port: u16,

    /// Shared secret for gateway-to-sandbox SSH handshake.
    #[serde(default)]
    pub ssh_handshake_secret: String,

    /// Allowed clock skew for SSH handshake validation, in seconds.
    #[serde(default = "default_ssh_handshake_skew_secs")]
    pub ssh_handshake_skew_secs: u64,

    /// TTL for SSH session tokens, in seconds. 0 disables expiry.
    #[serde(default = "default_ssh_session_ttl_secs")]
    pub ssh_session_ttl_secs: u64,

    /// Kubernetes secret name containing client TLS materials for sandbox pods.
    /// When set, sandbox pods get this secret mounted so they can connect to
    /// the server over mTLS.
    #[serde(default)]
    pub client_tls_secret_name: String,

    /// Host gateway IP for sandbox pod hostAliases.
    /// When set, sandbox pods get hostAliases entries mapping
    /// `host.docker.internal` and `host.openshell.internal` to this IP,
    /// allowing them to reach services running on the Docker host.
    #[serde(default)]
    pub host_gateway_ip: String,

    /// Sandbox backend: `"kubernetes"` (default) or `"apple-container"`.
    ///
    /// When set to `"apple-container"`, the server manages sandboxes via
    /// the container bridge daemon instead of the Kubernetes API.
    #[serde(default = "default_sandbox_backend")]
    pub sandbox_backend: String,

    /// Endpoint of the container bridge daemon (e.g., `https://host.containers.internal:50052`).
    ///
    /// Required when `sandbox_backend` is `"apple-container"`. The gateway
    /// connects to this endpoint over mutual TLS.
    #[serde(default)]
    pub bridge_endpoint: String,

    /// TLS configuration for the container bridge connection.
    ///
    /// When set, the gateway authenticates to the bridge daemon using mTLS.
    /// The gateway presents `bridge_tls.cert_path` / `bridge_tls.key_path`
    /// as the client certificate and verifies the bridge's server certificate
    /// against `bridge_tls.ca_path`.
    pub bridge_tls: Option<BridgeTlsConfig>,
}

/// TLS configuration.
///
/// By default mTLS is enforced — all clients must present a certificate
/// signed by the given CA.  When `allow_unauthenticated` is `true`, the
/// TLS handshake also accepts connections without a client certificate
/// (needed for reverse-proxy deployments like Cloudflare Tunnel).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Path to the TLS certificate file.
    pub cert_path: PathBuf,

    /// Path to the TLS private key file.
    pub key_path: PathBuf,

    /// Path to the CA certificate file for client certificate verification (mTLS).
    /// The server requires all clients to present a valid certificate signed by
    /// this CA.
    pub client_ca_path: PathBuf,

    /// When `true`, the TLS handshake succeeds even without a client
    /// certificate.  Application-layer middleware must then enforce auth
    /// (e.g. via a CF JWT header).
    #[serde(default)]
    pub allow_unauthenticated: bool,
}

/// TLS configuration for the gateway-to-bridge-daemon connection.
///
/// Both sides authenticate: the bridge daemon verifies the gateway's client
/// certificate, and the gateway verifies the bridge daemon's server certificate.
/// All certificates must be signed by the same CA.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeTlsConfig {
    /// Path to the CA certificate for verifying the bridge daemon's server cert.
    pub ca_path: PathBuf,

    /// Path to the client certificate the gateway presents to the bridge daemon.
    pub cert_path: PathBuf,

    /// Path to the private key for the client certificate.
    pub key_path: PathBuf,
}

impl Config {
    /// Create a new config with optional TLS.
    pub fn new(tls: Option<TlsConfig>) -> Self {
        Self {
            bind_address: default_bind_address(),
            log_level: default_log_level(),
            tls,
            database_url: String::new(),
            sandbox_namespace: default_sandbox_namespace(),
            sandbox_image: String::new(),
            sandbox_image_pull_policy: String::new(),
            grpc_endpoint: String::new(),
            ssh_gateway_host: default_ssh_gateway_host(),
            ssh_gateway_port: default_ssh_gateway_port(),
            ssh_connect_path: default_ssh_connect_path(),
            sandbox_ssh_port: default_sandbox_ssh_port(),
            ssh_handshake_secret: String::new(),
            ssh_handshake_skew_secs: default_ssh_handshake_skew_secs(),
            ssh_session_ttl_secs: default_ssh_session_ttl_secs(),
            client_tls_secret_name: String::new(),
            host_gateway_ip: String::new(),
            sandbox_backend: default_sandbox_backend(),
            bridge_endpoint: String::new(),
            bridge_tls: None,
        }
    }

    /// Create a new configuration with the given bind address.
    #[must_use]
    pub const fn with_bind_address(mut self, addr: SocketAddr) -> Self {
        self.bind_address = addr;
        self
    }

    /// Create a new configuration with the given log level.
    #[must_use]
    pub fn with_log_level(mut self, level: impl Into<String>) -> Self {
        self.log_level = level.into();
        self
    }

    /// Create a new configuration with a database URL.
    #[must_use]
    pub fn with_database_url(mut self, url: impl Into<String>) -> Self {
        self.database_url = url.into();
        self
    }

    /// Create a new configuration with a sandbox namespace.
    #[must_use]
    pub fn with_sandbox_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.sandbox_namespace = namespace.into();
        self
    }

    /// Create a new configuration with a default sandbox image.
    #[must_use]
    pub fn with_sandbox_image(mut self, image: impl Into<String>) -> Self {
        self.sandbox_image = image.into();
        self
    }

    /// Create a new configuration with a sandbox image pull policy.
    #[must_use]
    pub fn with_sandbox_image_pull_policy(mut self, policy: impl Into<String>) -> Self {
        self.sandbox_image_pull_policy = policy.into();
        self
    }

    /// Create a new configuration with a gRPC endpoint for sandbox callback.
    #[must_use]
    pub fn with_grpc_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.grpc_endpoint = endpoint.into();
        self
    }

    /// Create a new configuration with the SSH gateway host.
    #[must_use]
    pub fn with_ssh_gateway_host(mut self, host: impl Into<String>) -> Self {
        self.ssh_gateway_host = host.into();
        self
    }

    /// Create a new configuration with the SSH gateway port.
    #[must_use]
    pub const fn with_ssh_gateway_port(mut self, port: u16) -> Self {
        self.ssh_gateway_port = port;
        self
    }

    /// Create a new configuration with the SSH connect path.
    #[must_use]
    pub fn with_ssh_connect_path(mut self, path: impl Into<String>) -> Self {
        self.ssh_connect_path = path.into();
        self
    }

    /// Create a new configuration with the sandbox SSH port.
    #[must_use]
    pub const fn with_sandbox_ssh_port(mut self, port: u16) -> Self {
        self.sandbox_ssh_port = port;
        self
    }

    /// Create a new configuration with the SSH handshake secret.
    #[must_use]
    pub fn with_ssh_handshake_secret(mut self, secret: impl Into<String>) -> Self {
        self.ssh_handshake_secret = secret.into();
        self
    }

    /// Create a new configuration with SSH handshake skew allowance.
    #[must_use]
    pub const fn with_ssh_handshake_skew_secs(mut self, secs: u64) -> Self {
        self.ssh_handshake_skew_secs = secs;
        self
    }

    /// Create a new configuration with the SSH session TTL.
    #[must_use]
    pub const fn with_ssh_session_ttl_secs(mut self, secs: u64) -> Self {
        self.ssh_session_ttl_secs = secs;
        self
    }

    /// Set the Kubernetes secret name for sandbox client TLS materials.
    #[must_use]
    pub fn with_client_tls_secret_name(mut self, name: impl Into<String>) -> Self {
        self.client_tls_secret_name = name.into();
        self
    }

    /// Set the host gateway IP for sandbox pod hostAliases.
    #[must_use]
    pub fn with_host_gateway_ip(mut self, ip: impl Into<String>) -> Self {
        self.host_gateway_ip = ip.into();
        self
    }

    /// Set the sandbox backend (`"kubernetes"` or `"apple-container"`).
    #[must_use]
    pub fn with_sandbox_backend(mut self, backend: impl Into<String>) -> Self {
        self.sandbox_backend = backend.into();
        self
    }

    /// Set the container bridge daemon endpoint.
    #[must_use]
    pub fn with_bridge_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.bridge_endpoint = endpoint.into();
        self
    }

    /// Set the mTLS configuration for the bridge daemon connection.
    #[must_use]
    pub fn with_bridge_tls(mut self, tls: BridgeTlsConfig) -> Self {
        self.bridge_tls = Some(tls);
        self
    }
}

fn default_bind_address() -> SocketAddr {
    "0.0.0.0:8080".parse().expect("valid default address")
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_sandbox_namespace() -> String {
    "default".to_string()
}

fn default_ssh_gateway_host() -> String {
    "127.0.0.1".to_string()
}

const fn default_ssh_gateway_port() -> u16 {
    8080
}

fn default_ssh_connect_path() -> String {
    "/connect/ssh".to_string()
}

const fn default_sandbox_ssh_port() -> u16 {
    2222
}

const fn default_ssh_handshake_skew_secs() -> u64 {
    300
}

const fn default_ssh_session_ttl_secs() -> u64 {
    86400 // 24 hours
}

fn default_sandbox_backend() -> String {
    "kubernetes".to_string()
}
