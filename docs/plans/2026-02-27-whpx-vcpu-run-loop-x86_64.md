# WHPX vCPU 运行循环设计文档（第一阶段 - x86_64）

**日期**: 2026-02-27
**状态**: 设计阶段
**目标**: 实现 WHPX 后端的 x86_64 vCPU 运行循环，支持最小集合的 VM exits

---

## 概述

本文档描述 libkrun WHPX 后端 vCPU 运行循环的第一阶段实现。采用分阶段策略，首先实现 x86_64 架构的最小功能集合，验证架构设计的可行性。

**实现范围**:
- 架构: x86_64
- VM exits: MMIO read/write、IO port read/write、HLT、Shutdown
- 目标: 支持简单的 x86 Linux guest 启动

---

## 架构设计

### 核心组件

#### 1. WhpxVcpu 结构体

封装 WHPX vCPU handle 和相关状态：

```rust
pub struct WhpxVcpu {
    partition: WHV_PARTITION_HANDLE,
    index: u32,
}

impl WhpxVcpu {
    pub fn new(partition: WHV_PARTITION_HANDLE, index: u32) -> Result<Self>;
    pub fn run(&mut self) -> Result<VcpuExit>;
    pub fn set_registers(&mut self, regs: &Registers) -> Result<()>;
    pub fn get_registers(&mut self) -> Result<Registers>;
}
```

**职责**:
- 创建和管理 WHPX vCPU handle
- 调用 `WHvRunVirtualProcessor` 执行 guest 代码
- 解析 `WHV_RUN_VP_EXIT_CONTEXT` 并转换为 `VcpuExit` 枚举
- 提供寄存器读写接口

#### 2. VcpuExit 枚举

表示不同的 VM exit 原因：

```rust
pub enum VcpuExit<'a> {
    MmioRead(u64, &'a mut [u8]),
    MmioWrite(u64, &'a [u8]),
    IoPortRead(u16, &'a mut [u8]),
    IoPortWrite(u16, &'a [u8]),
    Halted,
    Shutdown,
}
```

**设计要点**:
- 使用生命周期参数避免数据拷贝
- 地址和数据直接引用 WHPX 提供的缓冲区
- 与 HVF 的 `VcpuExit` 保持相似的接口

#### 3. VcpuEmulation 枚举

表示 emulation 处理结果：

```rust
enum VcpuEmulation {
    Handled,    // 成功处理，继续运行
    Stopped,    // 虚拟机停止
    Halted,     // CPU 暂停，等待中断
}
```

#### 4. Registers 结构体

x86_64 寄存器状态：

```rust
pub struct Registers {
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
    pub cs: SegmentRegister,
    pub ds: SegmentRegister,
    pub ss: SegmentRegister,
    pub es: SegmentRegister,
    pub fs: SegmentRegister,
    pub gs: SegmentRegister,
}

pub struct SegmentRegister {
    pub selector: u16,
    pub base: u64,
    pub limit: u32,
    pub attributes: u16,
}
```

#### 5. Vcpu::run() 方法

主运行循环：

```rust
pub fn run(&mut self, init_tls_sender: Sender<bool>) {
    let mut whpx_vcpu = WhpxVcpu::new(self.partition, self.id as u32)
        .expect("Can't create WHPX vCPU");

    // 设置初始寄存器状态
    whpx_vcpu.set_initial_state(self.boot_entry_addr, ...)
        .expect("Can't set WHPX vCPU initial state");

    init_tls_sender.send(true).expect("Cannot notify vcpu TLS initialization.");

    loop {
        match self.run_emulation(&mut whpx_vcpu) {
            Ok(VcpuEmulation::Handled) => (),
            Ok(VcpuEmulation::Halted) => self.wait_for_interrupt(),
            Ok(VcpuEmulation::Stopped) => {
                self.exit(FC_EXIT_CODE_OK);
                break;
            }
            Err(_) => {
                self.exit(FC_EXIT_CODE_GENERIC_ERROR);
                break;
            }
        }
    }
}
```

---

## 数据流

### 1. 启动流程

```
Vcpu::start_threaded()
  └─> 创建 vCPU 线程
      └─> Vcpu::run()
          └─> WhpxVcpu::new(partition, index)
              └─> WHvCreateVirtualProcessor
          └─> whpx_vcpu.set_initial_state()
              └─> WHvSetVirtualProcessorRegisters
          └─> 发送 TLS 初始化信号
```

### 2. 运行循环

```
loop {
    whpx_vcpu.run()
      └─> WHvRunVirtualProcessor
          └─> 返回 WHV_RUN_VP_EXIT_CONTEXT

    解析 exit_context.ExitReason:
      - WHvRunVpExitReasonMemoryAccess
          └─> 提取地址、访问类型、数据
          └─> 返回 VcpuExit::MmioRead/Write

      - WHvRunVpExitReasonX64IoPortAccess
          └─> 提取端口、访问类型、数据
          └─> 返回 VcpuExit::IoPortRead/Write

      - WHvRunVpExitReasonX64Halt
          └─> 返回 VcpuExit::Halted

      - WHvRunVpExitReasonCanceled
          └─> 返回 VcpuExit::Shutdown

    run_emulation() 处理 VcpuExit:
      - MmioRead/Write → mmio_bus.read/write()
      - IoPortRead/Write → io_handler.read/write()
      - Halted → 返回 VcpuEmulation::Halted
      - Shutdown → 返回 VcpuEmulation::Stopped
}
```

### 3. Exit 处理详细流程

**MMIO 访问**:
```
Guest 执行 MMIO 读写
  └─> WHPX 拦截，返回 WHvRunVpExitReasonMemoryAccess
      └─> 提取 GPA (guest physical address)
      └─> 提取访问类型 (read/write)
      └─> 提取数据缓冲区
      └─> 转换为 VcpuExit::MmioRead/Write
          └─> mmio_bus.read/write(vcpu_id, addr, data)
              └─> 查找对应的设备
              └─> 调用设备的 read/write 方法
              └─> 数据写回 WHPX 缓冲区
          └─> 返回 VcpuEmulation::Handled
```

**IO Port 访问**:
```
Guest 执行 IN/OUT 指令
  └─> WHPX 拦截，返回 WHvRunVpExitReasonX64IoPortAccess
      └─> 提取端口号
      └─> 提取访问类型 (in/out)
      └─> 提取数据缓冲区
      └─> 转换为 VcpuExit::IoPortRead/Write
          └─> io_handler.read/write(port, data)
              └─> 处理标准 IO ports (0x3f8 串口等)
              └─> 数据写回 WHPX 缓冲区
          └─> 返回 VcpuEmulation::Handled
```

---

## 错误处理

### WHPX API 错误

所有 WHPX API 调用返回 `HRESULT`，使用 `windows-rs` 的错误处理机制：

```rust
WHvCreateVirtualProcessor(partition, index, 0)
    .map_err(|_| Error::VmSetup)?;
```

**关键错误场景**:

1. **vCPU 创建失败**:
   - 原因: partition 无效、索引超出范围、资源不足
   - 处理: 返回 `Error::VmSetup`，调用者负责清理

2. **运行失败**:
   - 原因: vCPU 状态异常、内存访问违规
   - 处理: 返回 `Error::VcpuRun`，退出运行循环

3. **寄存器访问失败**:
   - 原因: 寄存器名称无效、vCPU 未初始化
   - 处理: 返回 `Error::VcpuRun`

### Exit 处理错误

1. **未知的 exit reason**:
   ```rust
   _ => {
       warn!("Unknown exit reason: {:?}", exit_context.ExitReason);
       return Ok(VcpuEmulation::Stopped);
   }
   ```

2. **MMIO/IO port 访问无效地址**:
   ```rust
   if mmio_bus.read(addr, data).is_err() {
       error!("MMIO read failed at 0x{:x}", addr);
       // 继续运行，guest 会看到总线错误
   }
   ```

3. **寄存器状态不一致**:
   ```rust
   if registers.rip == 0 {
       panic!("Invalid RIP after exit");
   }
   ```

### 资源清理

```rust
impl Drop for WhpxVcpu {
    fn drop(&mut self) {
        unsafe {
            let _ = WHvDeleteVirtualProcessor(self.partition, self.index);
        }
    }
}
```

确保即使发生 panic 也能正确清理 vCPU handle。

---

## 测试策略

### 第一阶段：单元测试

```rust
#[cfg(all(test, target_os = "windows"))]
mod tests {
    #[test]
    fn test_whpx_vcpu_create() {
        let partition = create_test_partition();
        let vcpu = WhpxVcpu::new(partition, 0);
        assert!(vcpu.is_ok());
    }

    #[test]
    fn test_register_read_write() {
        let mut vcpu = create_test_vcpu();
        let mut regs = Registers::default();
        regs.rip = 0x1000;
        vcpu.set_registers(&regs).unwrap();

        let read_regs = vcpu.get_registers().unwrap();
        assert_eq!(read_regs.rip, 0x1000);
    }
}
```

**限制**:
- 需要 Windows 环境和 Hyper-V
- 交叉编译环境无法运行
- 使用 `#[cfg(all(test, target_os = "windows"))]` 标记

### 第二阶段：集成测试

**测试场景**:

1. **简单指令执行**:
   ```asm
   mov rax, 0x42
   hlt
   ```
   验证 vCPU 能执行指令并正确 halt。

2. **MMIO 访问**:
   ```asm
   mov rax, [0xfee00000]  ; LAPIC base
   ```
   验证 MMIO read exit 被正确处理。

3. **IO port 访问**:
   ```asm
   in al, 0x3f8  ; 串口
   ```
   验证 IO port read exit 被正确处理。

**测试环境**:
- Windows 10 2004+ 或 Windows 11
- Hyper-V 已启用
- 管理员权限

---

## 实现约束

### 技术约束

1. **WHPX API 限制**:
   - 需要 Windows 10 2004+ 或 Windows 11
   - 需要启用 Hyper-V 功能
   - 某些 API 在 aarch64 Windows 上可能不可用

2. **交叉编译限制**:
   - 从 Linux/macOS 交叉编译时无法运行测试
   - 需要在实际 Windows 机器上验证

3. **性能考虑**:
   - 每次 VM exit 都有上下文切换开销
   - MMIO/IO port 访问频繁时性能可能受影响
   - 第一阶段不做性能优化，专注功能正确性

### 设计约束

1. **与 HVF 保持一致**:
   - 使用相似的代码结构和命名
   - 便于维护和理解
   - 为后续 aarch64 实现提供模式

2. **最小功能集合**:
   - 只实现必需的 VM exits
   - 不包含 CPUID、MSR、中断注入
   - 后续阶段逐步添加

3. **错误处理优先**:
   - 所有 WHPX API 调用都要检查错误
   - 资源清理必须正确
   - 避免 handle 泄漏

---

## 后续阶段

### 第二阶段：扩展 x86_64 支持

- 添加 CPUID exit 处理
- 添加 MSR read/write 处理
- 实现中断注入
- 实现异常处理

### 第三阶段：aarch64 支持

- 实现 aarch64 vCPU 运行循环
- 处理 ARM64 特定的 exits（HVC、SMC、系统寄存器）
- 实现 GIC 中断控制器集成
- 实现 PSCI 支持

### 第四阶段：性能优化

- 批量 MMIO 处理
- 中断合并
- 减少上下文切换
- 优化寄存器访问

---

## 参考资料

- [WinHvPlatform API 文档](https://docs.microsoft.com/en-us/virtualization/api/)
- [windows-rs crate 文档](https://microsoft.github.io/windows-docs-rs/)
- macOS HVF 实现: `src/vmm/src/macos/vstate.rs`
- Linux KVM 实现: `src/vmm/src/linux/vstate.rs`

---

## 附录：WHPX API 映射

| 功能 | WHPX API | HVF API (参考) |
|------|----------|----------------|
| 创建 vCPU | `WHvCreateVirtualProcessor` | `hv_vcpu_create` |
| 运行 vCPU | `WHvRunVirtualProcessor` | `hv_vcpu_run` |
| 读寄存器 | `WHvGetVirtualProcessorRegisters` | `hv_vcpu_get_reg` |
| 写寄存器 | `WHvSetVirtualProcessorRegisters` | `hv_vcpu_set_reg` |
| 删除 vCPU | `WHvDeleteVirtualProcessor` | (自动清理) |

---

**设计完成日期**: 2026-02-27
**下一步**: 创建详细的实施计划
