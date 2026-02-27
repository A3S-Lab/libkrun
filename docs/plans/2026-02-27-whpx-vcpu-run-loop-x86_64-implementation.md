# WHPX vCPU 运行循环实施计划（第一阶段 - x86_64）

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 实现 WHPX 后端的 x86_64 vCPU 运行循环，支持最小集合的 VM exits（MMIO、IO port、HLT、Shutdown）

**Architecture:** 参考 macOS HVF 实现模式，创建 WhpxVcpu 结构体封装 WHPX vCPU handle，实现 run_emulation() 方法处理 VM exits，使用 VcpuEmulation 枚举表示处理结果。

**Tech Stack:** Rust, windows-rs (v0.58), WinHvPlatform API, WHPX

---

## Task 1: 创建 WhpxVcpu 模块骨架

**Files:**
- Create: `src/vmm/src/windows/whpx_vcpu.rs`
- Modify: `src/vmm/src/windows/mod.rs`

**Step 1: 创建 whpx_vcpu.rs 骨架**

创建文件 `src/vmm/src/windows/whpx_vcpu.rs`:

```rust
// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use windows::Win32::System::Hypervisor::*;

use super::vstate::Result;

/// WHPX vCPU wrapper
pub struct WhpxVcpu {
    partition: WHV_PARTITION_HANDLE,
    index: u32,
}

impl WhpxVcpu {
    /// Creates a new WHPX vCPU
    pub fn new(partition: WHV_PARTITION_HANDLE, index: u32) -> Result<Self> {
        // TODO: Call WHvCreateVirtualProcessor
        Ok(WhpxVcpu { partition, index })
    }

    /// Returns the vCPU index
    pub fn id(&self) -> u32 {
        self.index
    }
}

impl Drop for WhpxVcpu {
    fn drop(&mut self) {
        // TODO: Call WHvDeleteVirtualProcessor
    }
}
```

**Step 2: 在 mod.rs 中添加模块声明**

修改 `src/vmm/src/windows/mod.rs`:

```rust
pub mod vstate;
pub mod whpx_vcpu;
```

**Step 3: 提交**

```bash
git add src/vmm/src/windows/whpx_vcpu.rs src/vmm/src/windows/mod.rs
git commit -m "feat(vmm): create WhpxVcpu module skeleton

Add basic structure for WHPX vCPU wrapper.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 2: 实现 WhpxVcpu 创建和销毁

**Files:**
- Modify: `src/vmm/src/windows/whpx_vcpu.rs`

**Step 1: 实现 new() 方法**

在 `WhpxVcpu::new()` 中添加实现:

```rust
pub fn new(partition: WHV_PARTITION_HANDLE, index: u32) -> Result<Self> {
    unsafe {
        WHvCreateVirtualProcessor(partition, index, 0)
            .map_err(|_| super::vstate::Error::VmSetup)?;
    }
    Ok(WhpxVcpu { partition, index })
}
```

**Step 2: 实现 Drop trait**

在 `Drop::drop()` 中添加实现:

```rust
fn drop(&mut self) {
    unsafe {
        let _ = WHvDeleteVirtualProcessor(self.partition, self.index);
    }
}
```

**Step 3: 提交**

```bash
git add src/vmm/src/windows/whpx_vcpu.rs
git commit -m "feat(vmm): implement WhpxVcpu creation and cleanup

Implement WHvCreateVirtualProcessor and WHvDeleteVirtualProcessor calls.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 3: 添加 VcpuExit 和 VcpuEmulation 枚举

**Files:**
- Modify: `src/vmm/src/windows/whpx_vcpu.rs`

**Step 1: 添加 VcpuExit 枚举**

在文件顶部添加:

```rust
/// VM exit reasons
pub enum VcpuExit<'a> {
    /// MMIO read
    MmioRead(u64, &'a mut [u8]),
    /// MMIO write
    MmioWrite(u64, &'a [u8]),
    /// IO port read
    IoPortRead(u16, &'a mut [u8]),
    /// IO port write
    IoPortWrite(u16, &'a [u8]),
    /// CPU halted
    Halted,
    /// VM shutdown
    Shutdown,
}
```

**Step 2: 添加 VcpuEmulation 枚举**

```rust
/// Emulation result
pub(super) enum VcpuEmulation {
    /// Successfully handled, continue
    Handled,
    /// VM stopped
    Stopped,
    /// CPU halted, waiting for interrupt
    Halted,
}
```

**Step 3: 提交**

```bash
git add src/vmm/src/windows/whpx_vcpu.rs
git commit -m "feat(vmm): add VcpuExit and VcpuEmulation enums

Define VM exit types and emulation results for WHPX.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 4: 实现 WhpxVcpu::run() 方法骨架

**Files:**
- Modify: `src/vmm/src/windows/whpx_vcpu.rs`

**Step 1: 添加 run() 方法签名**

在 `WhpxVcpu` impl 块中添加:

```rust
/// Runs the vCPU and returns the exit reason
pub fn run(&mut self) -> Result<VcpuExit> {
    unsafe {
        let mut exit_context: WHV_RUN_VP_EXIT_CONTEXT = std::mem::zeroed();
        
        WHvRunVirtualProcessor(
            self.partition,
            self.index,
            &mut exit_context as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<WHV_RUN_VP_EXIT_CONTEXT>() as u32,
        )
        .map_err(|_| super::vstate::Error::VcpuRun)?;

        // TODO: Parse exit_context and return VcpuExit
        Ok(VcpuExit::Shutdown)
    }
}
```

**Step 2: 提交**

```bash
git add src/vmm/src/windows/whpx_vcpu.rs
git commit -m "feat(vmm): add WhpxVcpu::run() method skeleton

Add basic WHvRunVirtualProcessor call.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 5: 实现 VM exit 解析 - MMIO

**Files:**
- Modify: `src/vmm/src/windows/whpx_vcpu.rs`

**Step 1: 添加 exit context 解析逻辑**

替换 `run()` 方法中的 TODO:

```rust
// Parse exit reason
match exit_context.ExitReason {
    WHvRunVpExitReasonMemoryAccess => {
        let mem_access = &exit_context.Anonymous.MemoryAccess;
        let gpa = mem_access.Gpa;
        
        // Access type: 0 = read, 1 = write
        if mem_access.AccessInfo.Anonymous.AccessType() == 0 {
            // MMIO Read
            let data_slice = std::slice::from_raw_parts_mut(
                &mut exit_context.Anonymous.MemoryAccess.Anonymous.Data as *mut _ as *mut u8,
                mem_access.AccessInfo.Anonymous.AccessSize() as usize,
            );
            Ok(VcpuExit::MmioRead(gpa, data_slice))
        } else {
            // MMIO Write
            let data_slice = std::slice::from_raw_parts(
                &exit_context.Anonymous.MemoryAccess.Anonymous.Data as *const _ as *const u8,
                mem_access.AccessInfo.Anonymous.AccessSize() as usize,
            );
            Ok(VcpuExit::MmioWrite(gpa, data_slice))
        }
    }
    WHvRunVpExitReasonCanceled => Ok(VcpuExit::Shutdown),
    _ => {
        warn!("Unhandled exit reason: {:?}", exit_context.ExitReason);
        Ok(VcpuExit::Shutdown)
    }
}
```

**Step 2: 添加 log 依赖**

确保文件顶部有:

```rust
#[macro_use]
extern crate log;
```

或在 `use` 语句中添加:

```rust
use log::warn;
```

**Step 3: 提交**

```bash
git add src/vmm/src/windows/whpx_vcpu.rs
git commit -m "feat(vmm): implement MMIO exit parsing

Parse WHvRunVpExitReasonMemoryAccess and return VcpuExit::MmioRead/Write.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 6: 实现 VM exit 解析 - IO Port

**Files:**
- Modify: `src/vmm/src/windows/whpx_vcpu.rs`

**Step 1: 添加 IO port exit 处理**

在 `match exit_context.ExitReason` 中添加新分支（在 `WHvRunVpExitReasonCanceled` 之前）:

```rust
WHvRunVpExitReasonX64IoPortAccess => {
    let io_access = &exit_context.Anonymous.IoPortAccess;
    let port = io_access.PortNumber;
    let access_size = io_access.AccessInfo.Anonymous.AccessSize() as usize;
    
    // Access type: 0 = out (write), 1 = in (read)
    if io_access.AccessInfo.Anonymous.IsWrite() == 0 {
        // IO Port Read (IN instruction)
        let data_slice = std::slice::from_raw_parts_mut(
            &mut exit_context.Anonymous.IoPortAccess.Anonymous.Data as *mut _ as *mut u8,
            access_size,
        );
        Ok(VcpuExit::IoPortRead(port, data_slice))
    } else {
        // IO Port Write (OUT instruction)
        let data_slice = std::slice::from_raw_parts(
            &exit_context.Anonymous.IoPortAccess.Anonymous.Data as *const _ as *const u8,
            access_size,
        );
        Ok(VcpuExit::IoPortWrite(port, data_slice))
    }
}
```

**Step 2: 提交**

```bash
git add src/vmm/src/windows/whpx_vcpu.rs
git commit -m "feat(vmm): implement IO port exit parsing

Parse WHvRunVpExitReasonX64IoPortAccess and return VcpuExit::IoPortRead/Write.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 7: 实现 VM exit 解析 - HLT

**Files:**
- Modify: `src/vmm/src/windows/whpx_vcpu.rs`

**Step 1: 添加 HLT exit 处理**

在 `match exit_context.ExitReason` 中添加新分支:

```rust
WHvRunVpExitReasonX64Halt => Ok(VcpuExit::Halted),
```

**Step 2: 提交**

```bash
git add src/vmm/src/windows/whpx_vcpu.rs
git commit -m "feat(vmm): implement HLT exit parsing

Parse WHvRunVpExitReasonX64Halt and return VcpuExit::Halted.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 8: 在 Vcpu 中添加 run_emulation() 方法

**Files:**
- Modify: `src/vmm/src/windows/vstate.rs`

**Step 1: 导入 WhpxVcpu**

在文件顶部添加:

```rust
use super::whpx_vcpu::{VcpuEmulation, VcpuExit, WhpxVcpu};
```

**Step 2: 添加 run_emulation() 方法**

在 `Vcpu` impl 块中添加（在 `start_threaded()` 之后）:

```rust
/// Runs emulation for one VM exit
fn run_emulation(&mut self, whpx_vcpu: &mut WhpxVcpu) -> Result<VcpuEmulation> {
    let vcpuid = whpx_vcpu.id();

    match whpx_vcpu.run() {
        Ok(exit) => match exit {
            VcpuExit::MmioRead(addr, data) => {
                if let Some(ref mmio_bus) = self.mmio_bus {
                    debug!("vCPU {} MMIO read 0x{:x}", vcpuid, addr);
                    mmio_bus.read(vcpuid, addr, data);
                }
                Ok(VcpuEmulation::Handled)
            }
            VcpuExit::MmioWrite(addr, data) => {
                if let Some(ref mmio_bus) = self.mmio_bus {
                    debug!("vCPU {} MMIO write 0x{:x}", vcpuid, addr);
                    mmio_bus.write(vcpuid, addr, data);
                }
                Ok(VcpuEmulation::Handled)
            }
            VcpuExit::IoPortRead(port, data) => {
                debug!("vCPU {} IO port read 0x{:x}", vcpuid, port);
                // TODO: Implement IO port handling
                Ok(VcpuEmulation::Handled)
            }
            VcpuExit::IoPortWrite(port, data) => {
                debug!("vCPU {} IO port write 0x{:x}", vcpuid, port);
                // TODO: Implement IO port handling
                Ok(VcpuEmulation::Handled)
            }
            VcpuExit::Halted => {
                debug!("vCPU {} halted", vcpuid);
                Ok(VcpuEmulation::Halted)
            }
            VcpuExit::Shutdown => {
                info!("vCPU {} received shutdown signal", vcpuid);
                Ok(VcpuEmulation::Stopped)
            }
        },
        Err(e) => {
            error!("Error running WHPX vCPU: {:?}", e);
            Err(e)
        }
    }
}
```

**Step 3: 添加 log 宏导入**

确保文件顶部有:

```rust
use log::{debug, error, info};
```

**Step 4: 提交**

```bash
git add src/vmm/src/windows/vstate.rs
git commit -m "feat(vmm): add Vcpu::run_emulation() method

Implement VM exit handling for MMIO, IO port, HLT, and Shutdown.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 9: 实现 Vcpu::run() 主循环（x86_64）

**Files:**
- Modify: `src/vmm/src/windows/vstate.rs`

**Step 1: 添加 x86_64 特定的 Vcpu 构造函数**

在 `Vcpu` impl 块中添加:

```rust
#[cfg(target_arch = "x86_64")]
pub fn new_x86_64(
    id: u8,
    boot_entry_addr: vm_memory::GuestAddress,
    exit_evt: utils::eventfd::EventFd,
    vcpu_list: std::sync::Arc<devices::legacy::VcpuList>,
) -> Result<Self> {
    let (event_sender, event_receiver) = crossbeam_channel::unbounded();
    let (response_sender, response_receiver) = crossbeam_channel::unbounded();

    Ok(Vcpu {
        id,
        boot_entry_addr: boot_entry_addr.raw_value(),
        boot_receiver: None,
        boot_senders: None,
        fdt_addr: 0,
        mmio_bus: None,
        exit_evt,
        mpidr: 0,
        event_receiver,
        event_sender: Some(event_sender),
        response_receiver: Some(response_receiver),
        response_sender,
        vcpu_list,
        nested_enabled: false,
    })
}
```

**Step 2: 添加 x86_64 配置方法**

```rust
#[cfg(target_arch = "x86_64")]
pub fn configure_x86_64(&mut self) -> Result<()> {
    // TODO: Configure x86_64 specific registers
    Ok(())
}
```

**Step 3: 修改 start_threaded() 中的 run 循环**

替换 `start_threaded()` 中的线程体:

```rust
.spawn(move || {
    init_tls_sender
        .send(true)
        .expect("Cannot notify vcpu TLS initialization.");
    
    self.run();
})
```

**Step 4: 实现 run() 方法**

在 `Vcpu` impl 块中添加:

```rust
/// Main vCPU run loop
pub fn run(&mut self) {
    // Get partition handle from somewhere - for now use a placeholder
    // In real implementation, this should be passed from Vm
    let partition = unsafe { std::mem::zeroed() }; // TODO: Get from Vm
    
    let mut whpx_vcpu = WhpxVcpu::new(partition, self.id as u32)
        .expect("Can't create WHPX vCPU");

    // TODO: Set initial register state (RIP, RSP, etc.)

    loop {
        match self.run_emulation(&mut whpx_vcpu) {
            Ok(VcpuEmulation::Handled) => (),
            Ok(VcpuEmulation::Halted) => {
                // TODO: Wait for interrupt
                debug!("vCPU {} halted, waiting for interrupt", self.id);
            }
            Ok(VcpuEmulation::Stopped) => {
                self.exit(super::super::FC_EXIT_CODE_OK);
                break;
            }
            Err(_) => {
                self.exit(super::super::FC_EXIT_CODE_GENERIC_ERROR);
                break;
            }
        }
    }
}
```

**Step 5: 提交**

```bash
git add src/vmm/src/windows/vstate.rs
git commit -m "feat(vmm): implement Vcpu::run() main loop for x86_64

Add vCPU run loop with WhpxVcpu integration.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 10: 添加 partition handle 传递机制

**Files:**
- Modify: `src/vmm/src/windows/vstate.rs`

**Step 1: 在 Vcpu 结构体中添加 partition 字段**

修改 `Vcpu` 结构体:

```rust
pub struct Vcpu {
    id: u8,
    partition: WHV_PARTITION_HANDLE,  // 新增
    boot_entry_addr: u64,
    // ... 其他字段
}
```

**Step 2: 修改构造函数接受 partition**

修改 `new_x86_64()`:

```rust
pub fn new_x86_64(
    id: u8,
    partition: WHV_PARTITION_HANDLE,  // 新增参数
    boot_entry_addr: vm_memory::GuestAddress,
    exit_evt: utils::eventfd::EventFd,
    vcpu_list: std::sync::Arc<devices::legacy::VcpuList>,
) -> Result<Self> {
    // ...
    Ok(Vcpu {
        id,
        partition,  // 新增
        // ...
    })
}
```

同样修改 `new_aarch64()` 添加 partition 参数。

**Step 3: 在 run() 中使用 self.partition**

修改 `run()` 方法:

```rust
pub fn run(&mut self) {
    let mut whpx_vcpu = WhpxVcpu::new(self.partition, self.id as u32)
        .expect("Can't create WHPX vCPU");
    // ...
}
```

**Step 4: 提交**

```bash
git add src/vmm/src/windows/vstate.rs
git commit -m "feat(vmm): add partition handle to Vcpu

Pass partition handle from Vm to Vcpu for vCPU creation.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 11: 添加基础寄存器设置

**Files:**
- Modify: `src/vmm/src/windows/whpx_vcpu.rs`

**Step 1: 添加 set_initial_state() 方法**

在 `WhpxVcpu` impl 块中添加:

```rust
#[cfg(target_arch = "x86_64")]
pub fn set_initial_state(&mut self, rip: u64, rsp: u64) -> Result<()> {
    unsafe {
        // Set RIP
        let mut reg_names = [WHvX64RegisterRip];
        let mut reg_values: [WHV_REGISTER_VALUE; 1] = std::mem::zeroed();
        reg_values[0].Reg64 = rip;

        WHvSetVirtualProcessorRegisters(
            self.partition,
            self.index,
            &reg_names as *const _,
            1,
            &reg_values as *const _,
        )
        .map_err(|_| super::vstate::Error::VcpuRun)?;

        // Set RSP
        reg_names[0] = WHvX64RegisterRsp;
        reg_values[0].Reg64 = rsp;

        WHvSetVirtualProcessorRegisters(
            self.partition,
            self.index,
            &reg_names as *const _,
            1,
            &reg_values as *const _,
        )
        .map_err(|_| super::vstate::Error::VcpuRun)?;

        // Set RFLAGS (enable interrupts)
        reg_names[0] = WHvX64RegisterRflags;
        reg_values[0].Reg64 = 0x2; // IF flag

        WHvSetVirtualProcessorRegisters(
            self.partition,
            self.index,
            &reg_names as *const _,
            1,
            &reg_values as *const _,
        )
        .map_err(|_| super::vstate::Error::VcpuRun)?;

        Ok(())
    }
}
```

**Step 2: 在 Vcpu::run() 中调用**

修改 `vstate.rs` 中的 `run()` 方法:

```rust
let mut whpx_vcpu = WhpxVcpu::new(self.partition, self.id as u32)
    .expect("Can't create WHPX vCPU");

#[cfg(target_arch = "x86_64")]
whpx_vcpu
    .set_initial_state(self.boot_entry_addr, 0x8000)  // RSP = 32KB
    .expect("Can't set WHPX vCPU initial state");
```

**Step 3: 提交**

```bash
git add src/vmm/src/windows/whpx_vcpu.rs src/vmm/src/windows/vstate.rs
git commit -m "feat(vmm): add basic x86_64 register initialization

Set RIP, RSP, and RFLAGS for vCPU startup.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 12: 添加编译验证和文档

**Files:**
- Modify: `README.md`

**Step 1: 验证编译**

运行: `cargo check --package vmm`
预期: 编译通过（可能有警告）

**Step 2: 更新 README.md**

在 Windows 部分添加实现状态:

```markdown
### Windows (Experimental)
- Windows 10 2004+ or Windows 11
- Hyper-V enabled
- WinHvPlatform API support
- Architectures: x86_64, aarch64

**Implementation Status (x86_64)**:
- ✅ Basic infrastructure (Vm, Vcpu structures)
- ✅ vCPU run loop
- ✅ MMIO read/write exits
- ✅ IO port read/write exits
- ✅ HLT and Shutdown exits
- ⏳ CPUID exits (planned)
- ⏳ MSR exits (planned)
- ⏳ Interrupt injection (planned)
```

**Step 3: 提交**

```bash
git add README.md
git commit -m "docs: update Windows implementation status

Document completed x86_64 vCPU run loop features.

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## 验证清单

完成所有任务后，验证以下内容:

- [ ] `cargo check --package vmm` 通过
- [ ] WhpxVcpu 模块编译无错误
- [ ] Vcpu::run() 方法实现完整
- [ ] 所有 VM exits 都有处理逻辑
- [ ] 文档已更新
- [ ] 所有更改已提交

---

## 后续任务

### 第二阶段：扩展 x86_64 支持
- 实现 CPUID exit 处理
- 实现 MSR read/write 处理
- 实现中断注入
- 添加段寄存器完整配置

### 第三阶段：aarch64 支持
- 实现 aarch64 vCPU 运行循环
- 处理 ARM64 特定的 exits

### 第四阶段：测试和优化
- 添加单元测试
- 在实际 Windows 环境中测试
- 性能优化

---

## 注意事项

1. **交叉编译限制**: 从 macOS 交叉编译到 Windows 时，无法运行测试，只能验证编译通过
2. **WHPX API 文档**: 参考 Microsoft 官方文档了解 API 详细用法
3. **错误处理**: 所有 WHPX API 调用都要检查错误并正确转换
4. **资源清理**: 确保 Drop trait 正确实现，避免 handle 泄漏
5. **TODO 标记**: 代码中的 TODO 需要在后续阶段完成

