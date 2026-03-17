# Windows WHPX 内核启动项目总结

**日期:** 2026-03-18
**目标:** 在Windows WHPX虚拟化平台上启动Linux内核

---

## 项目状态

### ✅ 已完成的里程碑

1. **内核加载** (2026-03-17)
   - 成功从libkrunfw.dll加载19MB内核
   - 内核入口点：0x1000123

2. **高半部映射** (2026-03-17)
   - 实现3级页表配置
   - Identity mapping: 0x0-0x40000000 (1GB)
   - Higher-half mapping: 0xffffffff80000000+ → 0x0-0x40000000

3. **MMIO指令处理** (2026-03-17)
   - 实现手动从guest内存获取指令字节
   - 支持WHPX未提供指令字节的情况

4. **中断启用** (2026-03-18) 🎉
   - **关键修复:** 设置RFLAGS.IF标志
   - 内核现在可以接收中断
   - PIT定时器以100 Hz注入IRQ 0

5. **内核执行确认** (2026-03-18) 🎉
   - 内核成功启动并持续执行
   - 约81个VM exit在2秒内
   - RIP从0xffffffff81021fc2推进到循环地址

6. **循环分析工具** (2026-03-18)
   - 添加详细的循环地址日志
   - 记录GPA、访问类型、访问大小
   - 创建测试脚本和分析文档

### 🔄 当前状态

**内核在循环中执行**
- 循环地址：0xffffffff8102200e ↔ 0xffffffff81022010
- 所有VM exit都是MemoryAccess类型
- 中断正在注入但循环继续

**可能的原因:**
1. 自旋锁等待
2. 轮询设备寄存器
3. 等待中断处理完成
4. 内核空闲循环

### ⏳ 待完成的里程碑

1. **突破循环** - 确定循环原因并提供响应
2. **串口输出** - 看到内核的console输出
3. **完整启动** - 内核完成启动进入init
4. **运行nginx** - 最终目标

---

## 技术架构

### 虚拟化层次

```
┌─────────────────────────────────────┐
│         Linux Kernel (Guest)        │
│  (0xffffffff80000000+ higher-half)  │
├─────────────────────────────────────┤
│      WHPX vCPU (whpx_vcpu.rs)      │
│  - VM exit handling                 │
│  - MMIO/IO port emulation          │
│  - Instruction decoding             │
├─────────────────────────────────────┤
│   Windows Hypervisor Platform API  │
│  - WHvRunVirtualProcessor           │
│  - WHvRequestInterrupt              │
│  - WHvSetVirtualProcessorRegisters  │
├─────────────────────────────────────┤
│      Windows Hyper-V (Host)         │
└─────────────────────────────────────┘
```

### 关键组件

**1. 页表配置 (vstate.rs)**
- 3级页表：PML4 → PDPTE → PDE
- 支持identity和higher-half映射
- 启用PAE和long mode

**2. 中断处理 (builder.rs, whpx_vcpu.rs)**
- PIT定时器线程（100 Hz）
- WHvRequestInterrupt API
- irq_pending_evt信号机制

**3. MMIO模拟 (whpx_vcpu.rs)**
- IOAPIC stub (0xfec00000)
- LAPIC stub (0xfee00000)
- 串口设备 (0x3f8-0x3ff)

**4. 设备总线 (devices/)**
- MMIO bus for memory-mapped devices
- IO bus for port-mapped devices
- Serial, APIC, PIT等设备

---

## 关键代码修改

### 1. RFLAGS.IF 标志修复

**文件:** `src/vmm/src/windows/vstate.rs:372`

```rust
// 之前：只设置保留位
v[4].Reg64 = 0x2;

// 之后：启用中断标志
v[4].Reg64 = 0x2 | (1 << 9);  // bit 9 = IF (interrupt enable)
```

**影响:** 这是最关键的修复，使内核能够接收中断并开始执行。

### 2. 循环地址分析

**文件:** `src/vmm/src/windows/whpx_vcpu.rs:1050-1065`

```rust
// 检测并记录循环地址的详细信息
if rip == 0xffffffff8102200e || rip == 0xffffffff81022010 {
    let access_type_str = match access_type {
        0 => "Read", 1 => "Write", 2 => "Execute", _ => "Unknown",
    };
    // 记录GPA、类型、大小
    info!("🔍 LOOP #{}: RIP={:#x}, GPA={:#x}, Type={}, Size={}",
          LOOP_COUNT, rip, gpa, access_type_str, access_size);
}
```

**影响:** 帮助理解内核在循环中做什么，为下一步优化提供数据。

### 3. PIT定时器中断

**文件:** `src/vmm/src/builder.rs`

```rust
// 启动100 Hz定时器线程
std::thread::spawn(move || {
    loop {
        std::thread::sleep(Duration::from_millis(10));
        intc_clone.lock().unwrap().set_irq(Some(0), None);
    }
});
```

**影响:** 提供周期性中断，驱动内核调度器。

---

## 调试工具

### 1. 日志级别

```bash
# Trace - 所有VM exit
RUST_LOG=trace cargo run --release --example test_kernel_boot

# Info - 重要事件和循环分析
RUST_LOG=info cargo run --release --example test_kernel_boot

# Debug - MMIO访问和中断
RUST_LOG=debug cargo run --release --example test_kernel_boot
```

### 2. 测试脚本

**PowerShell脚本:** `run_test.ps1`
- 自动运行测试
- 保存日志到文件
- 提取关键信息（循环、进度、定时器）

### 3. 分析命令

```bash
# 查看循环地址访问
grep "LOOP #" test_output.log | head -20

# 查看vCPU进度
grep "progress" test_output.log

# 查看定时器中断
grep "PIT timer" test_output.log

# 查看MMIO访问
grep "MMIO access" test_output.log
```

---

## 文档

### 核心文档

1. **BREAKTHROUGH.md** - 重大突破记录
   - RFLAGS.IF修复
   - 内核执行确认
   - 当前状态分析

2. **DEBUG_FINDINGS.md** - 详细调试发现
   - 问题分析
   - 技术细节
   - 下一步计划

3. **LOOP_ANALYSIS.md** - 循环分析
   - 分析工具说明
   - 预期输出
   - 分析方法

### 代码文档

- `src/vmm/src/windows/whpx_vcpu.rs` - WHPX vCPU实现
- `src/vmm/src/windows/vstate.rs` - vCPU状态管理
- `src/vmm/src/builder.rs` - VM构建和设备配置

---

## 下一步计划

### 短期（本周）

1. **运行循环分析** (1小时)
   - 收集循环地址的GPA和访问类型
   - 确定是否是自旋锁或轮询

2. **反汇编循环代码** (2小时)
   - 从guest内存读取指令
   - 理解循环的实际逻辑

3. **提供响应** (1-2天)
   - 根据分析结果实现相应的设备响应
   - 或者修复中断传递问题

### 中期（本月）

1. **串口输出** (2-3天)
   - 确保串口设备正确工作
   - 看到内核的console输出

2. **完整启动** (1周)
   - 让内核完成启动序列
   - 进入init进程

3. **rootfs和init** (1周)
   - 创建完整的rootfs
   - 实现init脚本

### 长期（下月）

1. **网络设备** (1-2周)
   - 实现virtio-net或其他网络设备
   - 配置网络栈

2. **运行nginx** (1周)
   - 安装nginx到rootfs
   - 配置并启动nginx
   - 验证HTTP服务

---

## 性能指标

### 当前性能

- **VM exit频率:** ~40 exits/秒（在循环中）
- **中断注入:** 100 Hz (每秒100次)
- **内核执行:** 持续运行，无panic或崩溃

### 预期性能

- **启动时间:** < 5秒（从加载到init）
- **串口输出:** < 1秒延迟
- **网络吞吐:** > 100 Mbps
- **HTTP请求:** < 10ms延迟

---

## 已知问题

### 1. 运行环境

**问题:** Git Bash中缺少DLL
**解决:** 使用PowerShell或Windows命令提示符

### 2. 构建依赖

**问题:** cargo clean删除libkrunfw.dll
**解决:** 重新构建libkrunfw-win

### 3. 循环状态

**问题:** 内核在循环中，未继续启动
**状态:** 正在分析，已添加详细日志

---

## 贡献者

- 主要开发：Claude (AI Assistant)
- 项目指导：用户
- 基础代码：libkrun项目

---

## 参考资料

### 技术文档

- [Windows Hypervisor Platform API](https://docs.microsoft.com/en-us/virtualization/api/)
- [Intel x86_64 Architecture Manual](https://www.intel.com/content/www/us/en/architecture-and-technology/64-ia-32-architectures-software-developer-manual-325462.html)
- [Linux Kernel Boot Protocol](https://www.kernel.org/doc/html/latest/x86/boot.html)

### 相关项目

- [libkrun](https://github.com/containers/libkrun) - 原始项目
- [Firecracker](https://github.com/firecracker-microvm/firecracker) - 类似的microVM项目
- [Cloud Hypervisor](https://github.com/cloud-hypervisor/cloud-hypervisor) - Rust虚拟化项目

---

## 许可证

本项目遵循libkrun的许可证（Apache 2.0）。
