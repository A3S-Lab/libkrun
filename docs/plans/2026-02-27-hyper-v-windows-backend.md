# Hyper-V Windows 后端支持实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**目标:** 为 libkrun 增加基于 WinHvPlatform (WHPX) 的 Windows 后端支持

**架构:** 参照现有 macOS HVF 后端的设计模式，创建 Windows 平台的虚拟化后端，使用 windows-rs crate 绑定 WinHvPlatform API，支持 x86_64 和 aarch64 架构。

**技术栈:**
- Rust (edition 2021)
- windows-rs crate (v0.58+)
- WinHvPlatform API (Windows 10 2004+)
- 交叉编译支持 (x86_64-pc-windows-msvc, aarch64-pc-windows-msvc)

---

## Task 1: 添加 Windows 依赖到 Cargo.toml

**文件:**
- Modify: `src/vmm/Cargo.toml`

**步骤 1: 添加 windows-rs 依赖**

在 `[target.'cfg(target_os = "macos")'.dependencies]` 后面添加:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.58", features = [
    "Win32_System_Hypervisor",
    "Win32_Foundation",
    "Win32_System_Memory",
] }
```

**步骤 2: 验证依赖配置**

运行: `cargo metadata --format-version 1 | grep -A 5 "windows"`
预期: 显示 windows crate 配置信息

**步骤 3: 提交更改**

```bash
git add src/vmm/Cargo.toml
git commit -m "feat(vmm): add windows-rs dependency for WHPX backend

Add windows crate with Win32_System_Hypervisor feature for WinHvPlatform API bindings.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 2: 创建 Windows vstate 模块骨架

**文件:**
- Create: `src/vmm/src/windows/mod.rs`
- Create: `src/vmm/src/windows/vstate.rs`

**步骤 1: 创建 windows 模块目录**

运行: `mkdir -p src/vmm/src/windows`

**步骤 2: 创建 mod.rs**

```rust
// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

pub mod vstate;
```

**步骤 3: 创建 vstate.rs 骨架**

```rust
// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::{Display, Formatter};
use std::result;

use windows::Win32::System::Hypervisor::*;

/// Errors associated with WHPX operations
#[derive(Debug)]
pub enum Error {
    /// Invalid guest memory configuration
    GuestMemoryMmap(vm_memory::GuestMemoryError),
    /// Cannot set the memory regions
    SetUserMemoryRegion,
    /// Cannot configure the microvm
    VmSetup,
    /// Cannot run the VCPUs
    VcpuRun,
    /// Cannot spawn a new vCPU thread
    VcpuSpawn(std::io::Error),
    /// Vcpu not present in TLS
    VcpuTlsNotPresent,
    /// Cannot cleanly initialize vcpu TLS
    VcpuTlsInit,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Error::GuestMemoryMmap(e) => write!(f, "Guest memory error: {e:?}"),
            Error::SetUserMemoryRegion => write!(f, "Cannot set the memory regions"),
            Error::VmSetup => write!(f, "Cannot configure the microvm"),
            Error::VcpuRun => write!(f, "Cannot run the VCPUs"),
            Error::VcpuSpawn(e) => write!(f, "Cannot spawn a new vCPU thread: {e}"),
            Error::VcpuTlsNotPresent => write!(f, "Vcpu not present in TLS"),
            Error::VcpuTlsInit => write!(f, "Cannot clean init vcpu TLS"),
        }
    }
}

pub type Result<T> = result::Result<T, Error>;

/// A wrapper around creating and using a WHPX VM
pub struct Vm {
    // TODO: Add WHPX partition handle
}

impl Vm {
    /// Constructs a new `Vm` using WHPX
    pub fn new(_nested_enabled: bool) -> Result<Self> {
        // TODO: Call WHvCreatePartition
        Ok(Vm {})
    }

    /// Initializes the guest memory
    pub fn memory_init(&mut self, _guest_mem: &vm_memory::GuestMemoryMmap) -> Result<()> {
        // TODO: Call WHvMapGpaRange for each memory region
        Ok(())
    }
}

/// A wrapper around creating and using a WHPX VCPU
pub struct Vcpu {
    id: u8,
}

impl Vcpu {
    /// Constructs a new VCPU for WHPX
    pub fn new_aarch64(
        id: u8,
        _boot_entry_addr: vm_memory::GuestAddress,
        _boot_receiver: Option<crossbeam_channel::Receiver<u64>>,
        _exit_evt: utils::eventfd::EventFd,
        _vcpu_list: std::sync::Arc<devices::legacy::VcpuList>,
        _nested_enabled: bool,
    ) -> Result<Self> {
        Ok(Vcpu { id })
    }

    /// Returns the cpu index
    pub fn cpu_index(&self) -> u8 {
        self.id
    }
}

/// Wrapper over Vcpu that hides the underlying interactions
pub struct VcpuHandle {
    // TODO: Add event channels
}

impl VcpuHandle {
    pub fn new(
        _event_sender: crossbeam_channel::Sender<VcpuEvent>,
        _response_receiver: crossbeam_channel::Receiver<VcpuResponse>,
        _vcpu_thread: std::thread::JoinHandle<()>,
    ) -> Self {
        Self {}
    }
}

#[derive(Debug)]
pub enum VcpuEvent {
    Pause,
    Resume,
}

#[derive(Debug, Eq, PartialEq)]
pub enum VcpuResponse {
    Paused,
    Resumed,
    Exited(u8),
}
```

**步骤 4: 验证编译**

运行: `cargo check --target x86_64-pc-windows-msvc --package vmm`
预期: 编译通过（可能有警告）

**步骤 5: 提交更改**

```bash
git add src/vmm/src/windows/
git commit -m "feat(vmm): create Windows vstate module skeleton

Add basic structure for WHPX-based virtualization backend.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 3: 在 vmm/lib.rs 中集成 Windows 后端

**文件:**
- Modify: `src/vmm/src/lib.rs:27-37`

**步骤 1: 添加 Windows 模块声明**

在第 32 行 `mod macos;` 后面添加:

```rust
#[cfg(target_os = "windows")]
mod windows;
```

**步骤 2: 添加 Windows vstate 导入**

在第 37 行 `use macos::vstate;` 后面添加:

```rust
#[cfg(target_os = "windows")]
use windows::vstate;
```

**步骤 3: 验证编译**

运行: `cargo check --target x86_64-pc-windows-msvc --package vmm`
预期: 编译通过

**步骤 4: 提交更改**

```bash
git add src/vmm/src/lib.rs
git commit -m "feat(vmm): integrate Windows vstate module

Add conditional compilation for Windows platform.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 4: 创建 Windows MMIO 设备管理器

**文件:**
- Create: `src/vmm/src/device_manager/whpx/mod.rs`
- Create: `src/vmm/src/device_manager/whpx/mmio.rs`

**步骤 1: 创建 whpx 目录**

运行: `mkdir -p src/vmm/src/device_manager/whpx`

**步骤 2: 创建 mod.rs**

```rust
// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

pub mod mmio;
```

**步骤 3: 创建 mmio.rs (复制 HVF 版本)**

从 `src/vmm/src/device_manager/hvf/mmio.rs` 复制内容，修改导入:

```rust
// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::{fmt, io};

use devices::fdt::DeviceInfoForFDT;
use devices::legacy::IrqChip;
use devices::{BusDevice, DeviceType};
use kernel::cmdline as kernel_cmdline;
use polly::event_manager::EventManager;
#[cfg(target_arch = "aarch64")]
use utils::eventfd::EventFd;

use crate::vstate::Vm;

// ... (rest of the file same as HVF version)
```

**步骤 4: 验证编译**

运行: `cargo check --target x86_64-pc-windows-msvc --package vmm`
预期: 编译通过

**步骤 5: 提交更改**

```bash
git add src/vmm/src/device_manager/whpx/
git commit -m "feat(vmm): create WHPX MMIO device manager

Add device manager for Windows WHPX backend, based on HVF implementation.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 5: 在 device_manager/mod.rs 中集成 WHPX

**文件:**
- Modify: `src/vmm/src/device_manager/mod.rs:14-22`

**步骤 1: 添加 WHPX 模块声明**

在第 22 行后面添加:

```rust
#[cfg(target_os = "windows")]
pub mod whpx;
#[cfg(target_os = "windows")]
pub use self::whpx::mmio;
```

**步骤 2: 验证编译**

运行: `cargo check --target x86_64-pc-windows-msvc --package vmm`
预期: 编译通过

**步骤 3: 提交更改**

```bash
git add src/vmm/src/device_manager/mod.rs
git commit -m "feat(vmm): integrate WHPX device manager

Add conditional compilation for Windows MMIO device manager.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 6: 实现 WHPX Vm 结构体

**文件:**
- Modify: `src/vmm/src/windows/vstate.rs:26-50`

**步骤 1: 添加 Vm 字段**

```rust
use windows::Win32::System::Hypervisor::*;
use windows::core::HRESULT;

pub struct Vm {
    partition: WHV_PARTITION_HANDLE,
}

impl Vm {
    pub fn new(_nested_enabled: bool) -> Result<Self> {
        unsafe {
            let mut partition: WHV_PARTITION_HANDLE = std::mem::zeroed();
            let hr = WHvCreatePartition(&mut partition);
            
            if hr.is_err() {
                return Err(Error::VmSetup);
            }

            // Set processor count
            let mut property = WHV_PARTITION_PROPERTY {
                ProcessorCount: 1,
                ..Default::default()
            };
            let hr = WHvSetPartitionProperty(
                partition,
                WHV_PARTITION_PROPERTY_CODE_ProcessorCount,
                &property as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<WHV_PARTITION_PROPERTY>() as u32,
            );

            if hr.is_err() {
                WHvDeletePartition(partition);
                return Err(Error::VmSetup);
            }

            // Setup partition
            let hr = WHvSetupPartition(partition);
            if hr.is_err() {
                WHvDeletePartition(partition);
                return Err(Error::VmSetup);
            }

            Ok(Vm { partition })
        }
    }

    pub fn memory_init(&mut self, guest_mem: &vm_memory::GuestMemoryMmap) -> Result<()> {
        use vm_memory::{GuestMemory, GuestMemoryRegion};

        for region in guest_mem.iter() {
            let host_addr = guest_mem
                .get_host_address(region.start_addr())
                .ok_or(Error::SetUserMemoryRegion)?;
            
            unsafe {
                let hr = WHvMapGpaRange(
                    self.partition,
                    host_addr as *const std::ffi::c_void,
                    region.start_addr().raw_value(),
                    region.len(),
                    WHV_MAP_GPA_RANGE_FLAGS_Read 
                        | WHV_MAP_GPA_RANGE_FLAGS_Write 
                        | WHV_MAP_GPA_RANGE_FLAGS_Execute,
                );

                if hr.is_err() {
                    return Err(Error::SetUserMemoryRegion);
                }
            }
        }

        Ok(())
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        unsafe {
            WHvDeletePartition(self.partition);
        }
    }
}
```

**步骤 2: 验证编译**

运行: `cargo check --target x86_64-pc-windows-msvc --package vmm`
预期: 编译通过

**步骤 3: 提交更改**

```bash
git add src/vmm/src/windows/vstate.rs
git commit -m "feat(vmm): implement WHPX Vm structure

Implement partition creation, memory mapping, and cleanup.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 7: 实现 WHPX Vcpu 结构体 (aarch64)

**文件:**
- Modify: `src/vmm/src/windows/vstate.rs:80-150`

**步骤 1: 添加 Vcpu 字段和实现**

```rust
pub struct Vcpu {
    id: u8,
    partition: WHV_PARTITION_HANDLE,
    boot_entry_addr: u64,
    boot_receiver: Option<crossbeam_channel::Receiver<u64>>,
    fdt_addr: u64,
    mmio_bus: Option<devices::Bus>,
    exit_evt: utils::eventfd::EventFd,
    mpidr: u64,
    event_receiver: crossbeam_channel::Receiver<VcpuEvent>,
    event_sender: Option<crossbeam_channel::Sender<VcpuEvent>>,
    response_receiver: Option<crossbeam_channel::Receiver<VcpuResponse>>,
    response_sender: crossbeam_channel::Sender<VcpuResponse>,
    vcpu_list: std::sync::Arc<devices::legacy::VcpuList>,
    nested_enabled: bool,
}

impl Vcpu {
    pub fn new_aarch64(
        id: u8,
        boot_entry_addr: vm_memory::GuestAddress,
        boot_receiver: Option<crossbeam_channel::Receiver<u64>>,
        exit_evt: utils::eventfd::EventFd,
        vcpu_list: std::sync::Arc<devices::legacy::VcpuList>,
        nested_enabled: bool,
    ) -> Result<Self> {
        use crossbeam_channel::unbounded;

        let (event_sender, event_receiver) = unbounded();
        let (response_sender, response_receiver) = unbounded();

        // TODO: Get partition handle from somewhere
        let partition: WHV_PARTITION_HANDLE = unsafe { std::mem::zeroed() };

        Ok(Vcpu {
            id,
            partition,
            boot_entry_addr: boot_entry_addr.raw_value(),
            boot_receiver,
            fdt_addr: 0,
            mmio_bus: None,
            exit_evt,
            mpidr: id as u64,
            event_receiver,
            event_sender: Some(event_sender),
            response_receiver: Some(response_receiver),
            response_sender,
            vcpu_list,
            nested_enabled,
        })
    }

    pub fn cpu_index(&self) -> u8 {
        self.id
    }

    pub fn get_mpidr(&self) -> u64 {
        self.mpidr
    }

    pub fn set_mmio_bus(&mut self, mmio_bus: devices::Bus) {
        self.mmio_bus = Some(mmio_bus);
    }

    pub fn configure_aarch64(&mut self, mem_info: &arch::ArchMemoryInfo) -> Result<()> {
        self.fdt_addr = mem_info.fdt_addr;
        Ok(())
    }

    pub fn start_threaded(mut self) -> Result<VcpuHandle> {
        use crossbeam_channel::unbounded;

        let event_sender = self.event_sender.take().unwrap();
        let response_receiver = self.response_receiver.take().unwrap();
        let (init_tls_sender, init_tls_receiver) = unbounded();

        let vcpu_thread = std::thread::Builder::new()
            .name(format!("fc_vcpu {}", self.cpu_index()))
            .spawn(move || {
                init_tls_sender.send(true).unwrap();
                // TODO: Implement run loop
            })
            .map_err(Error::VcpuSpawn)?;

        init_tls_receiver.recv().unwrap();

        Ok(VcpuHandle::new(
            event_sender,
            response_receiver,
            vcpu_thread,
        ))
    }
}
```

**步骤 2: 验证编译**

运行: `cargo check --target aarch64-pc-windows-msvc --package vmm`
预期: 编译通过（可能有警告）

**步骤 3: 提交更改**

```bash
git add src/vmm/src/windows/vstate.rs
git commit -m "feat(vmm): implement WHPX Vcpu structure for aarch64

Add vCPU creation and configuration for ARM64 Windows.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 8: 添加 libkrun build.rs Windows 支持

**文件:**
- Modify: `src/libkrun/build.rs`

**步骤 1: 添加 Windows 链接参数**

在文件末尾添加:

```rust
#[cfg(target_os = "windows")]
{
    println!("cargo:rustc-cdylib-link-arg=/DEF:libkrun.def");
    println!("cargo:rustc-link-lib=WinHvPlatform");
}
```

**步骤 2: 验证编译**

运行: `cargo check --target x86_64-pc-windows-msvc --package libkrun`
预期: 编译通过

**步骤 3: 提交更改**

```bash
git add src/libkrun/build.rs
git commit -m "feat(libkrun): add Windows build configuration

Link WinHvPlatform library for WHPX support.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 9: 创建交叉编译配置

**文件:**
- Create: `.cargo/config.toml`

**步骤 1: 创建 .cargo 目录**

运行: `mkdir -p .cargo`

**步骤 2: 创建 config.toml**

```toml
[target.x86_64-pc-windows-msvc]
linker = "lld-link"

[target.aarch64-pc-windows-msvc]
linker = "lld-link"
```

**步骤 3: 验证配置**

运行: `cargo build --target x86_64-pc-windows-msvc --package vmm --lib`
预期: 开始编译（可能失败，但配置已生效）

**步骤 4: 提交更改**

```bash
git add .cargo/config.toml
git commit -m "feat: add cross-compilation configuration for Windows

Configure linker for Windows MSVC targets.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 10: 文档更新

**文件:**
- Modify: `README.md`

**步骤 1: 添加 Windows 支持说明**

在 README.md 的 "Supported Platforms" 部分添加:

```markdown
### Windows (Experimental)
- Windows 10 2004+ or Windows 11
- Hyper-V enabled
- WinHvPlatform API support
- Architectures: x86_64, aarch64
```

**步骤 2: 添加构建说明**

```markdown
### Building for Windows

Cross-compile from Linux/macOS:
\`\`\`bash
cargo build --target x86_64-pc-windows-msvc --release
cargo build --target aarch64-pc-windows-msvc --release
\`\`\`

Native build on Windows:
\`\`\`powershell
cargo build --release
\`\`\`
```

**步骤 3: 提交更改**

```bash
git add README.md
git commit -m "docs: add Windows platform support documentation

Document WHPX backend requirements and build instructions.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## 后续任务 (未详细展开)

以下任务需要在基础架构完成后进一步实现:

### Task 11: 实现 WHPX vCPU 运行循环
- 实现 `WHvRunVirtualProcessor` 调用
- 处理 VM exits (MMIO, IO port, CPUID, MSR)
- 实现中断注入

### Task 12: 实现 x86_64 特定支持
- CPUID 配置
- MSR 处理
- IO port exits
- 段寄存器配置

### Task 13: 实现 aarch64 特定支持
- 系统寄存器配置
- GIC 中断控制器集成
- PSCI 支持

### Task 14: 集成测试
- 创建 Windows CI 工作流
- 添加单元测试
- 添加集成测试

### Task 15: 性能优化
- 批量 MMIO 处理
- 中断合并
- 内存映射优化

---

## 验证清单

完成所有任务后，验证以下内容:

- [ ] `cargo check --target x86_64-pc-windows-msvc` 通过
- [ ] `cargo check --target aarch64-pc-windows-msvc` 通过
- [ ] 所有条件编译正确
- [ ] 文档完整
- [ ] 所有更改已提交

---

## 注意事项

1. **交叉编译限制**: 从 Linux/macOS 交叉编译到 Windows 时，无法运行测试
2. **API 可用性**: aarch64 Windows 上的 WHPX API 可能有限制，需要在实际硬件上测试
3. **错误处理**: WHPX API 返回 HRESULT，需要正确转换为 Rust Error
4. **内存安全**: 所有 WHPX API 调用都是 unsafe，需要仔细处理

