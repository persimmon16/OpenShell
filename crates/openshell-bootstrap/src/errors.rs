// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Gateway error detection and user-friendly guidance.
//!
//! This module analyzes error messages and process logs to detect known
//! failure patterns and provide actionable recovery guidance.

/// A diagnosed gateway failure with user-friendly guidance.
#[derive(Debug, Clone)]
pub struct GatewayFailureDiagnosis {
    /// Short summary of what went wrong.
    pub summary: String,
    /// Detailed explanation of the issue.
    pub explanation: String,
    /// Commands or steps the user can take to fix the issue.
    pub recovery_steps: Vec<RecoveryStep>,
    /// Whether the issue might be auto-recoverable by retrying.
    pub retryable: bool,
}

/// A recovery step with a command and description.
#[derive(Debug, Clone)]
pub struct RecoveryStep {
    /// Description of what this step does.
    pub description: String,
    /// Command to run (if applicable).
    pub command: Option<String>,
}

impl RecoveryStep {
    fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            command: None,
        }
    }

    fn with_command(description: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            command: Some(command.into()),
        }
    }
}

/// How multiple matchers should be combined.
#[derive(Debug, Clone, Copy, Default)]
enum MatchMode {
    /// Match if ANY of the matchers is found (default).
    #[default]
    Any,
}

/// Known failure patterns and their detection logic.
struct FailurePattern {
    /// Patterns to match in error message or logs.
    matchers: &'static [&'static str],
    /// How to combine multiple matchers (default: Any).
    match_mode: MatchMode,
    /// Function to generate diagnosis.
    diagnose: fn(gateway_name: &str) -> GatewayFailureDiagnosis,
}

const FAILURE_PATTERNS: &[FailurePattern] = &[
    // Port already in use
    FailurePattern {
        matchers: &["port is already allocated", "address already in use"],
        match_mode: MatchMode::Any,
        diagnose: diagnose_port_conflict,
    },
    // Network connectivity issues (DNS, timeouts, unreachable)
    FailurePattern {
        matchers: &[
            "no such host",
            "i/o timeout",
            "network is unreachable",
            "connection refused",
            "connection reset by peer",
            "TLS handshake timeout",
            "no route to host",
            "temporary failure in name resolution",
        ],
        match_mode: MatchMode::Any,
        diagnose: diagnose_network_connectivity,
    },
    // TLS/certificate issues
    FailurePattern {
        matchers: &[
            "certificate has expired",
            "x509: certificate",
            "tls: failed to verify",
        ],
        match_mode: MatchMode::Any,
        diagnose: diagnose_certificate_issue,
    },
    // Apple Container not running or not installed (macOS)
    FailurePattern {
        matchers: &[
            "Apple Container is not running",
            "container system info",
            "container system start",
        ],
        match_mode: MatchMode::Any,
        diagnose: diagnose_apple_container_not_running,
    },
    // Apple Container vmnet network failure
    FailurePattern {
        matchers: &["vmnet", "network interface", "virtualization framework"],
        match_mode: MatchMode::Any,
        diagnose: diagnose_apple_container_vmnet,
    },
];

fn diagnose_port_conflict(_gateway_name: &str) -> GatewayFailureDiagnosis {
    GatewayFailureDiagnosis {
        summary: "Port already in use".to_string(),
        explanation: "The gateway port is already in use by another process."
            .to_string(),
        recovery_steps: vec![
            RecoveryStep::with_command(
                "Check what's using the port",
                "lsof -i :8080 || netstat -an | grep 8080",
            ),
            RecoveryStep::with_command(
                "Use a different port",
                "openshell gateway start --port 8081",
            ),
            RecoveryStep::with_command(
                "Or stop other openshell gateways",
                "openshell gateway list && openshell gateway destroy --name <name>",
            ),
        ],
        retryable: false,
    }
}

fn diagnose_network_connectivity(_gateway_name: &str) -> GatewayFailureDiagnosis {
    GatewayFailureDiagnosis {
        summary: "Network connectivity issue".to_string(),
        explanation: "Could not establish a network connection. This could be a DNS resolution \
            failure, firewall blocking the connection, or general internet connectivity issue."
            .to_string(),
        recovery_steps: vec![
            RecoveryStep::new("Check your internet connection"),
            RecoveryStep::with_command("Test DNS resolution", "nslookup ghcr.io"),
            RecoveryStep::with_command("Test registry connectivity", "curl -I https://ghcr.io/v2/"),
            RecoveryStep::new("Then retry: openshell gateway start"),
        ],
        retryable: true,
    }
}

fn diagnose_certificate_issue(gateway_name: &str) -> GatewayFailureDiagnosis {
    GatewayFailureDiagnosis {
        summary: "TLS certificate issue".to_string(),
        explanation: "There's a problem with the gateway's TLS certificates, possibly expired \
            or mismatched certificates from a previous installation."
            .to_string(),
        recovery_steps: vec![RecoveryStep::with_command(
            "Destroy and recreate the gateway to regenerate certificates",
            format!("openshell gateway destroy --name {gateway_name} && openshell gateway start"),
        )],
        retryable: false,
    }
}

fn diagnose_apple_container_not_running(_gateway_name: &str) -> GatewayFailureDiagnosis {
    GatewayFailureDiagnosis {
        summary: "Apple Container is not running".to_string(),
        explanation: "The Apple Container runtime is not running or not installed. \
            OpenShell on macOS uses Apple Container for lightweight VM-based sandboxes."
            .to_string(),
        recovery_steps: vec![
            RecoveryStep::with_command("Start Apple Container", "container system start"),
            RecoveryStep::new(
                "If not installed, install Apple Container from https://github.com/apple/container",
            ),
            RecoveryStep::new("Requires macOS 15 (Sequoia) or later"),
            RecoveryStep::new("Then retry: openshell gateway start"),
        ],
        retryable: true,
    }
}

fn diagnose_apple_container_vmnet(_gateway_name: &str) -> GatewayFailureDiagnosis {
    GatewayFailureDiagnosis {
        summary: "Apple Container networking failure".to_string(),
        explanation: "The vmnet networking framework used by Apple Container encountered \
            an error. This can happen after macOS updates or when the Virtualization \
            framework is in a bad state."
            .to_string(),
        recovery_steps: vec![
            RecoveryStep::with_command(
                "Restart the Apple Container service",
                "container system stop && container system start",
            ),
            RecoveryStep::new("If the issue persists, restart your Mac"),
            RecoveryStep::new("Then retry: openshell gateway start"),
        ],
        retryable: true,
    }
}

/// Analyze an error message and process logs to diagnose the failure.
///
/// Returns `Some(diagnosis)` if a known pattern is detected, `None` otherwise.
pub fn diagnose_failure(
    gateway_name: &str,
    error_message: &str,
    container_logs: Option<&str>,
) -> Option<GatewayFailureDiagnosis> {
    let combined = container_logs.map_or_else(
        || error_message.to_string(),
        |logs| format!("{error_message}\n{logs}"),
    );

    for pattern in FAILURE_PATTERNS {
        let matches = match pattern.match_mode {
            MatchMode::Any => pattern.matchers.iter().any(|m| combined.contains(m)),
        };
        if matches {
            return Some((pattern.diagnose)(gateway_name));
        }
    }

    None
}

/// Create a generic diagnosis when no specific pattern is matched.
pub fn generic_failure_diagnosis(gateway_name: &str) -> GatewayFailureDiagnosis {
    GatewayFailureDiagnosis {
        summary: "Gateway failed to start".to_string(),
        explanation: "The gateway encountered an unexpected error during startup.".to_string(),
        recovery_steps: vec![
            RecoveryStep::with_command(
                "Check server logs for details",
                format!("openshell doctor logs --name {gateway_name}"),
            ),
            RecoveryStep::with_command(
                "Run diagnostics",
                format!("openshell doctor check --name {gateway_name}"),
            ),
            RecoveryStep::with_command(
                "Try destroying and recreating the gateway",
                format!(
                    "openshell gateway destroy --name {gateway_name} && openshell gateway start"
                ),
            ),
            RecoveryStep::new(
                "If the issue persists, report it at https://github.com/nvidia/openshell/issues",
            ),
        ],
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnose_port_conflict() {
        let diagnosis = diagnose_failure("test", "port is already allocated", None);
        assert!(diagnosis.is_some());
        let d = diagnosis.unwrap();
        assert!(d.summary.contains("Port"));
    }

    #[test]
    fn test_no_match_returns_none() {
        let diagnosis = diagnose_failure("test", "some random error", Some("random logs"));
        assert!(diagnosis.is_none());
    }

    #[test]
    fn test_diagnose_apple_container_not_running() {
        let diagnosis = diagnose_failure("test", "Apple Container is not running", None);
        assert!(diagnosis.is_some());
        let d = diagnosis.unwrap();
        assert!(d.summary.contains("Apple Container"));
        assert!(d.retryable);
    }

    #[test]
    fn test_diagnose_network_connectivity() {
        let diagnosis = diagnose_failure(
            "test",
            "connection refused",
            None,
        );
        assert!(diagnosis.is_some());
        let d = diagnosis.unwrap();
        assert!(d.summary.contains("Network"));
    }

    // -- generic_failure_diagnosis tests --

    #[test]
    fn generic_diagnosis_suggests_doctor_logs() {
        let d = generic_failure_diagnosis("my-gw");
        let commands: Vec<String> = d
            .recovery_steps
            .iter()
            .filter_map(|s| s.command.clone())
            .collect();
        assert!(
            commands.iter().any(|c| c.contains("openshell doctor logs")),
            "expected 'openshell doctor logs' in recovery commands, got: {commands:?}"
        );
    }

    #[test]
    fn generic_diagnosis_suggests_doctor_check() {
        let d = generic_failure_diagnosis("my-gw");
        let commands: Vec<String> = d
            .recovery_steps
            .iter()
            .filter_map(|s| s.command.clone())
            .collect();
        assert!(
            commands
                .iter()
                .any(|c| c.contains("openshell doctor check")),
            "expected 'openshell doctor check' in recovery commands, got: {commands:?}"
        );
    }

    #[test]
    fn generic_diagnosis_includes_gateway_name() {
        let d = generic_failure_diagnosis("custom-name");
        let all_text: String = d
            .recovery_steps
            .iter()
            .filter_map(|s| s.command.clone())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            all_text.contains("custom-name"),
            "expected gateway name in recovery commands, got: {all_text}"
        );
    }
}
