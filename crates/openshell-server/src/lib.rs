// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `OpenShell` Server library.
//!
//! This crate provides the server implementation for `OpenShell`, including:
//! - gRPC service implementation
//! - HTTP health endpoints
//! - Protocol multiplexing (gRPC + HTTP on same port)
//! - mTLS support

mod auth;
mod grpc;
mod http;
mod inference;
mod multiplex;
mod persistence;
mod sandbox;
mod sandbox_index;
mod sandbox_watch;
mod ssh_tunnel;
mod tls;
pub mod tracing_bus;
mod ws_tunnel;

use openshell_core::{Config, Error, Result};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tracing::{debug, error, info};

pub use grpc::OpenShellService;
pub use http::{health_router, http_router};
pub use multiplex::{MultiplexService, MultiplexedService};
use persistence::Store;
use sandbox::SandboxBackend;
use sandbox_index::SandboxIndex;
use sandbox_watch::SandboxWatchBus;
pub use tls::TlsAcceptor;
use tracing_bus::TracingLogBus;

/// Server state shared across handlers.
#[derive(Debug)]
pub struct ServerState {
    /// Server configuration.
    pub config: Config,

    /// Persistence store.
    pub store: Arc<Store>,

    /// Sandbox backend — Apple Container.
    pub sandbox_backend: SandboxBackend,

    /// In-memory sandbox correlation index.
    pub sandbox_index: SandboxIndex,

    /// In-memory bus for sandbox update notifications.
    pub sandbox_watch_bus: SandboxWatchBus,

    /// In-memory bus for server process logs.
    pub tracing_log_bus: TracingLogBus,

    /// Active SSH tunnel connection counts per session token.
    pub ssh_connections_by_token: Mutex<HashMap<String, u32>>,

    /// Active SSH tunnel connection counts per sandbox id.
    pub ssh_connections_by_sandbox: Mutex<HashMap<String, u32>>,

    /// Serializes settings mutations (global and sandbox) to prevent
    /// read-modify-write races. Held for the duration of any setting
    /// set/delete operation, including the precedence check on sandbox
    /// mutations that reads global state.
    pub settings_mutex: tokio::sync::Mutex<()>,
}

/// Listen for shutdown signals (SIGTERM/SIGINT on Unix, Ctrl-C on others).
///
/// Returns a receiver that closes when shutdown is initiated.
#[cfg(unix)]
async fn listen_for_shutdown() -> broadcast::Receiver<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let (tx, rx) = broadcast::channel(1);
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");

    tokio::spawn(async move {
        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM");
            }
            _ = sigint.recv() => {
                info!("Received SIGINT");
            }
        }
        let _ = tx.send(());
    });

    rx
}

#[cfg(not(unix))]
async fn listen_for_shutdown() -> broadcast::Receiver<()> {
    let (tx, rx) = broadcast::channel(1);

    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        info!("Received Ctrl-C");
        let _ = tx.send(());
    });

    rx
}

fn is_benign_tls_handshake_failure(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset
    )
}

impl ServerState {
    /// Create new server state.
    #[must_use]
    pub fn new(
        config: Config,
        store: Arc<Store>,
        sandbox_backend: SandboxBackend,
        sandbox_index: SandboxIndex,
        sandbox_watch_bus: SandboxWatchBus,
        tracing_log_bus: TracingLogBus,
    ) -> Self {
        Self {
            config,
            store,
            sandbox_backend,
            sandbox_index,
            sandbox_watch_bus,
            tracing_log_bus,
            ssh_connections_by_token: Mutex::new(HashMap::new()),
            ssh_connections_by_sandbox: Mutex::new(HashMap::new()),
            settings_mutex: tokio::sync::Mutex::new(()),
        }
    }
}

/// Run the `OpenShell` server.
///
/// This starts a multiplexed gRPC/HTTP server on the configured bind address.
///
/// # Errors
///
/// Returns an error if the server fails to start or encounters a fatal error.
pub async fn run_server(config: Config, tracing_log_bus: TracingLogBus) -> Result<()> {
    let database_url = config.database_url.trim();
    if database_url.is_empty() {
        return Err(Error::config("database_url is required"));
    }
    if config.ssh_handshake_secret.is_empty() {
        return Err(Error::config(
            "ssh_handshake_secret is required. Set --ssh-handshake-secret or OPENSHELL_SSH_HANDSHAKE_SECRET",
        ));
    }

    let store = Store::connect(database_url).await?;

    // Initialize the Apple Container sandbox backend.
    info!("Initializing Apple Container sandbox backend");
    let sandbox_backend =
        SandboxBackend::AppleContainer(sandbox::apple_container::AppleContainerSandboxClient::new(
            config.sandbox_image.clone(),
            config.grpc_endpoint.clone(),
            format!("0.0.0.0:{}", config.sandbox_ssh_port),
            config.ssh_handshake_secret.clone(),
            config.ssh_handshake_skew_secs,
        ));

    let store = Arc::new(store);

    let sandbox_index = SandboxIndex::new();
    let sandbox_watch_bus = SandboxWatchBus::new();
    let state = Arc::new(ServerState::new(
        config.clone(),
        store.clone(),
        sandbox_backend,
        sandbox_index,
        sandbox_watch_bus,
        tracing_log_bus,
    ));

    ssh_tunnel::spawn_session_reaper(store.clone(), std::time::Duration::from_secs(3600));

    // Create the multiplexed service
    let service = MultiplexService::new(state.clone());

    // Bind the TCP listener
    let listener = TcpListener::bind(config.bind_address)
        .await
        .map_err(|e| Error::transport(format!("failed to bind to {}: {e}", config.bind_address)))?;

    info!(address = %config.bind_address, "Server listening");

    // Build TLS acceptor when TLS is configured; otherwise serve plaintext.
    let tls_acceptor = if let Some(tls) = &config.tls {
        Some(TlsAcceptor::from_files(
            &tls.cert_path,
            &tls.key_path,
            &tls.client_ca_path,
            tls.allow_unauthenticated,
        )?)
    } else {
        info!("TLS disabled — accepting plaintext connections");
        None
    };

    // Accept connections with graceful shutdown support
    let mut shutdown_rx = listen_for_shutdown().await;

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, addr) = match accept_result {
                    Ok(conn) => conn,
                    Err(e) => {
                        error!(error = %e, "Failed to accept connection");
                        continue;
                    }
                };

                let service = service.clone();

                if let Some(ref acceptor) = tls_acceptor {
                    let tls_acceptor = acceptor.clone();
                    tokio::spawn(async move {
                        match tls_acceptor.inner().accept(stream).await {
                            Ok(tls_stream) => {
                                if let Err(e) = service.serve(tls_stream).await {
                                    error!(error = %e, client = %addr, "Connection error");
                                }
                            }
                            Err(e) => {
                                if is_benign_tls_handshake_failure(&e) {
                                    debug!(error = %e, client = %addr, "TLS handshake closed early");
                                } else {
                                    error!(error = %e, client = %addr, "TLS handshake failed");
                                }
                            }
                        }
                    });
                } else {
                    tokio::spawn(async move {
                        if let Err(e) = service.serve(stream).await {
                            error!(error = %e, client = %addr, "Connection error");
                        }
                    });
                }
            }
            _ = shutdown_rx.recv() => {
                info!("Shutdown signal received, stopping acceptance of new connections");
                break;
            }
        }
    }

    info!("Graceful shutdown complete");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_benign_tls_handshake_failure;
    use std::io::{Error, ErrorKind};

    #[test]
    fn classifies_probe_style_tls_disconnects_as_benign() {
        for kind in [ErrorKind::UnexpectedEof, ErrorKind::ConnectionReset] {
            let error = Error::new(kind, "probe disconnected");
            assert!(is_benign_tls_handshake_failure(&error));
        }
    }

    #[test]
    fn preserves_real_tls_failures_as_errors() {
        for kind in [
            ErrorKind::InvalidData,
            ErrorKind::PermissionDenied,
            ErrorKind::Other,
        ] {
            let error = Error::new(kind, "real tls failure");
            assert!(!is_benign_tls_handshake_failure(&error));
        }
    }
}
