// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

pub fn container_name(name: &str) -> String {
    format!("openshell-cluster-{name}")
}
