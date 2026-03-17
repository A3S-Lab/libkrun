# Windows WHPX 内核启动项目

> 在Windows Hypervisor Platform上启动Linux内核的实验性项目

[![Status](https://img.shields.io/badge/status-active-success.svg)]()
[![Platform](https://img.shields.io/badge/platform-Windows%2011-blue.svg)]()
[![Hypervisor](https://img.shields.io/badge/hypervisor-WHPX-orange.svg)]()

---

## 🎯 项目目标

在Windows WHPX虚拟化平台上成功启动Linux内核，最终运行nginx Web服务器。

## ✨ 当前状态

### ✅ 已完成的里程碑

- [x] **内核加载** - 成功加载19MB Linux内核
- [x] **高半部映射** - 实现3级页表支持0xffffffff80000000+地址
- [x] **中断启用** - 修复RFLAGS.IF标志，启用中断机制
- [x] **内核执行** - 内核持续运行，响应中断
- [x] **调试工具** - 循环地址分析和详细日志

### 🔄 当前进展

**内核在循环中执行**
- 循环地址：`0xffffffff8102200e` ↔ `0xffffffff81022010`
- VM exit频率：约40次/秒
- 中断注入：100 Hz PIT定时器
- 状态：稳定运行，无崩溃

### ⏳ 下一步

1. **突破循环** - 分析循环原因并提供响应
2. **串口输出** - 获取内核console输出
3. **完整启动** - 进入init进程
4. **运行nginx** - 最终目标

---

## 🚀 快速开始

### 前置要求

- Windows 11（需要Hyper-V支持）
- Rust工具链（stable）
- PowerShell 5.0+

### 构建

```bash
# 1. 构建libkrunfw（包含内核）
cd src/libkrunfw-win
cargo build --release
cd ../..

# 2. 构建测试程序
cargo build --release --example test_kernel_boot
```

### 运行测试

```powershell
# 使用PowerShell脚本（推荐）
.\run_test.ps1

# 或手动运行
$env:RUST_LOG = "info"
.\target\release\examples\test_kernel_boot.exe
```

### 查看结果

```bash
# 循环地址分析
grep "LOOP #" test_output.log | head -20

# vCPU进度
grep "progress" test_output.log

# 定时器中断
grep "PIT timer" test_output.log
```

---

## 📚 文档

### 入门文档

- **[DOCS_INDEX.md](DOCS_INDEX.md)** - 📖 文档导航索引
- **[DEBUGGING_GUIDE.md](DEBUGGING_GUIDE.md)** - 🔧 调试指南（从这里开始！）
- **[PROJECT_SUMMARY.md](PROJECT_SUMMARY.md)** - 📊 项目完整总结

### 技术文档

- **[BREAKTHROUGH.md](BREAKTHROUGH.md)** - 🎉 重大突破记录
- **[DEBUG_FINDINGS.md](DEBUG_FINDINGS.md)** - 🔍 详细调试发现
- **[LOOP_ANALYSIS.md](LOOP_ANALYSIS.md)** - 🔄 循环分析工具

### 其他文档

- **[SESSION_SUMMARY.md](SESSION_SUMMARY.md)** - 📝 最新会话记录
- **[NGINX_REQUIREMENTS.md](NGINX_REQUIREMENTS.md)** - 🌐 Nginx运行需求分析

---

## 🏗️ 技术架构

```
┌─────────────────────────────────────┐
│      Linux Kernel (Guest)           │
│   Higher-half: 0xffffffff80000000+  │
├─────────────────────────────────────┤
│   WHPX vCPU (whpx_vcpu.rs)         │
│   - VM exit handling                │
│   - MMIO/IO emulation               │
│   - Instruction decoding            │
├─────────────────────────────────────┤
│   Windows Hypervisor Platform API  │
│   - WHvRunVirtualProcessor          │
│   - WHvRequestInterrupt             │
├─────────────────────────────────────┤
│   Windows Hyper-V (Host)            │
└─────────────────────────────────────┘
```

### 关键组件

- **页表配置** (`vstate.rs`) - 3级页表，identity + higher-half映射
- **中断处理** (`builder.rs`) - PIT定时器，100 Hz IRQ注入
- **MMIO模拟** (`whpx_vcpu.rs`) - IOAPIC/LAPIC stub，串口设备
- **设备总线** (`devices/`) - MMIO/IO总线，设备管理

---

## 🔑 关键突破

### RFLAGS.IF 标志修复

**问题：** 内核无法接收中断，因为RFLAGS.IF标志未设置

**修复：** `src/vmm/src/windows/vstate.rs:372`

```rust
// 之前：只设置保留位
v[4].Reg64 = 0x2;

// 之后：启用中断标志
v[4].Reg64 = 0x2 | (1 << 9);  // bit 9 = IF
```

**影响：** 这是最关键的修复，使内核能够接收中断并开始执行！

### 循环地址分析

**功能：** 自动检测和记录循环地址的详细信息

**代码：** `src/vmm/src/windows/whpx_vcpu.rs:1050-1065`

```rust
if rip == 0xffffffff8102200e || rip == 0xffffffff81022010 {
    info!("🔍 LOOP #{}: RIP={:#x}, GPA={:#x}, Type={}, Size={}",
          count, rip, gpa, access_type, access_size);
}
```

**用途：** 帮助理解内核在循环中的行为，为突破循环提供数据

---

## 📊 项目统计

### 代码

- **核心修改：** 2个文件（vstate.rs, whpx_vcpu.rs）
- **关键代码：** 约30行
- **影响：** 从"内核不响应"到"内核持续执行"

### 文档

- **文档数量：** 8个主要文档
- **总行数：** 1600+ 行
- **总字数：** 17000+ 字

### 性能

- **VM exit频率：** ~40 exits/秒
- **中断注入：** 100 Hz
- **内核状态：** 稳定运行

---

## 🛠️ 开发工具

### 日志级别

```bash
# TRACE - 所有VM exit详情
RUST_LOG=trace

# INFO - 关键事件和循环分析（推荐）
RUST_LOG=info

# DEBUG - MMIO访问和设备I/O
RUST_LOG=debug
```

### 分析命令

```bash
# 查看循环模式
grep "WHPX exit" trace.log | grep "0xffffffff8102200"

# 统计VM exit
grep "WHPX exit" trace.log | wc -l

# 查看MMIO访问
grep "MMIO access" debug.log
```

---

## 🤝 贡献

欢迎贡献！当前最需要的帮助：

1. **运行测试** - 在不同Windows版本上测试
2. **分析循环** - 确定循环原因
3. **实现响应** - 突破循环继续启动
4. **文档改进** - 补充和完善文档

### 开发流程

1. Fork项目
2. 创建特性分支 (`git checkout -b feature/amazing-feature`)
3. 提交更改 (`git commit -m 'feat: add amazing feature'`)
4. 推送到分支 (`git push origin feature/amazing-feature`)
5. 创建Pull Request

---

## 📝 提交规范

使用[Conventional Commits](https://www.conventionalcommits.org/)：

- `feat:` - 新功能
- `fix:` - Bug修复
- `docs:` - 文档更新
- `chore:` - 构建/工具更改
- `refactor:` - 代码重构

---

## 🐛 已知问题

### 运行环境

- **Git Bash DLL问题** - 使用PowerShell或Windows命令提示符
- **构建依赖** - cargo clean后需重新构建libkrunfw

### 内核状态

- **循环未突破** - 内核在两个地址间循环
- **无串口输出** - 尚未看到console输出

详见 [DEBUG_FINDINGS.md](DEBUG_FINDINGS.md)

---

## 📖 参考资料

### 技术文档

- [Windows Hypervisor Platform API](https://docs.microsoft.com/en-us/virtualization/api/)
- [Intel x86_64 Manual](https://www.intel.com/content/www/us/en/architecture-and-technology/64-ia-32-architectures-software-developer-manual-325462.html)
- [Linux Boot Protocol](https://www.kernel.org/doc/html/latest/x86/boot.html)

### 相关项目

- [libkrun](https://github.com/containers/libkrun) - 原始项目
- [Firecracker](https://github.com/firecracker-microvm/firecracker) - microVM
- [Cloud Hypervisor](https://github.com/cloud-hypervisor/cloud-hypervisor) - Rust VMM

---

## 📜 许可证

本项目遵循libkrun的许可证（Apache 2.0）。

---

## 🎉 致谢

- **libkrun项目** - 提供基础代码
- **Windows Hypervisor Platform** - 虚拟化API
- **所有贡献者** - 测试和反馈

---

## 📞 联系方式

- **Issues** - [GitHub Issues](https://github.com/containers/libkrun/issues)
- **Discussions** - [GitHub Discussions](https://github.com/containers/libkrun/discussions)

---

**最后更新：** 2026-03-18
**项目状态：** 🔄 活跃开发中
**下一个里程碑：** 突破内核循环

---

<div align="center">

**[开始使用](DEBUGGING_GUIDE.md)** | **[查看文档](DOCS_INDEX.md)** | **[了解进展](BREAKTHROUGH.md)**

Made with ❤️ for Windows virtualization

</div>
