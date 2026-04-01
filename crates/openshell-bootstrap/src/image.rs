// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Image reference constants and parsing utilities.

/// Image tag baked in at compile time.
///
/// Set via `OPENSHELL_IMAGE_TAG` env var during `cargo build`:
/// - Defaults to `"dev"` when unset (local builds).
/// - CI sets this explicitly: `"dev"` for main-branch builds, the version
///   string (e.g. `"0.6.0"`) for tagged releases.
pub const DEFAULT_IMAGE_TAG: &str = match option_env!("OPENSHELL_IMAGE_TAG") {
    Some(tag) => tag,
    None => "dev",
};

// ---------------------------------------------------------------------------
// GHCR registry defaults
// ---------------------------------------------------------------------------

/// Default registry host for pulling images.
pub const DEFAULT_REGISTRY: &str = "ghcr.io";

/// Default image repository base on GHCR (without component name or tag).
pub const DEFAULT_IMAGE_REPO_BASE: &str = "ghcr.io/nvidia/openshell";

/// Default full gateway image path on GHCR (without tag).
pub const DEFAULT_GATEWAY_IMAGE: &str = "ghcr.io/nvidia/openshell/cluster";

/// Parse an image reference into (repository, tag).
///
/// Examples:
/// - `nginx:latest` -> ("nginx", "latest")
/// - `nginx` -> ("nginx", "latest")
/// - `ghcr.io/org/repo:v1.0` -> ("ghcr.io/org/repo", "v1.0")
pub fn parse_image_ref(image_ref: &str) -> (String, String) {
    // Handle digest references (sha256:...)
    if image_ref.contains('@') {
        // For digest references, don't split - return the whole thing
        return (image_ref.to_string(), String::new());
    }

    // Find the last colon that's after any registry/path separators
    // This handles cases like "registry.io:5000/image:tag"
    if let Some(last_colon) = image_ref.rfind(':') {
        let before_colon = &image_ref[..last_colon];
        let after_colon = &image_ref[last_colon + 1..];

        // If there's a slash after this colon, it's a port not a tag
        if !after_colon.contains('/') {
            return (before_colon.to_string(), after_colon.to_string());
        }
    }

    // No tag found, default to "latest"
    (image_ref.to_string(), "latest".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_image() {
        let (repo, tag) = parse_image_ref("nginx:latest");
        assert_eq!(repo, "nginx");
        assert_eq!(tag, "latest");
    }

    #[test]
    fn parse_image_no_tag() {
        let (repo, tag) = parse_image_ref("nginx");
        assert_eq!(repo, "nginx");
        assert_eq!(tag, "latest");
    }

    #[test]
    fn parse_image_with_registry() {
        let (repo, tag) = parse_image_ref("ghcr.io/org/repo:v1.0");
        assert_eq!(repo, "ghcr.io/org/repo");
        assert_eq!(tag, "v1.0");
    }

    #[test]
    fn parse_image_with_registry_port() {
        let (repo, tag) = parse_image_ref("registry.io:5000/image:v1");
        assert_eq!(repo, "registry.io:5000/image");
        assert_eq!(tag, "v1");
    }

    #[test]
    fn parse_image_with_registry_port_no_tag() {
        let (repo, tag) = parse_image_ref("registry.io:5000/image");
        assert_eq!(repo, "registry.io:5000/image");
        assert_eq!(tag, "latest");
    }

    #[test]
    fn parse_image_with_digest() {
        let (repo, tag) = parse_image_ref("nginx@sha256:abc123");
        assert_eq!(repo, "nginx@sha256:abc123");
        assert_eq!(tag, "");
    }

    #[test]
    fn default_constants_are_consistent() {
        assert!(
            DEFAULT_GATEWAY_IMAGE.starts_with(DEFAULT_IMAGE_REPO_BASE),
            "gateway image should be under the default repo base"
        );
        assert!(
            DEFAULT_IMAGE_REPO_BASE.starts_with(DEFAULT_REGISTRY),
            "repo base should start with the registry host"
        );
    }
}
