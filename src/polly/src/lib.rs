// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod event_manager;
#[cfg(target_os = "windows")]
#[path = "event_manager_windows.rs"]
pub mod event_manager;
