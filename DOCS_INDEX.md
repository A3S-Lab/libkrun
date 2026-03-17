# Windows WHPX 内核启动项目文档索引

本项目致力于在Windows WHPX虚拟化平台上启动Linux内核，最终目标是运行nginx。

---

## 📚 文档导航

### 快速开始

- **[DEBUGGING_GUIDE.md](DEBUGGING_GUIDE.md)** - 🚀 从这里开始！
  - 如何构建和运行测试
  - 日志级别说明
  - 输出解读方法
  - 常见问题解答

### 项目概览

- **[PROJECT_SUMMARY.md](PROJECT_SUMMARY.md)** - 📊 完整项目总结
  - 已完成的里程碑
  - 技术架构图
  - 关键代码修改
  - 短/中/长期计划
  - 性能指标和已知问题

### 技术文档

- **[BREAKTHROUGH.md](BREAKTHROUGH.md)** - 🎉 重大突破记录
  - RFLAGS.IF修复（关键！）
  - 内核执行确认
  - 当前循环状态分析

- **[DEBUG_FINDINGS.md](DEBUG_FINDINGS.md)** - 🔍 详细调试发现
  - 问题分析
  - 技术细节
  - 中断传递流程
  - 页表配置

- **[LOOP_ANALYSIS.md](LOOP_ANALYSIS.md)** - 🔄 循环分析工具
  - 分析工具说明
  - 预期输出示例
  - 分析方法指南
  - 下一步行动

- **[RIP_TRACKING_GUIDE.md](RIP_TRACKING_GUIDE.md)** - 🎯 RIP跟踪指南 (最新!)
  - 全面的RIP执行跟踪
  - 自动卡住检测
  - 运行测试的详细步骤
  - 输出分析方法
  - 故障排除指南

### 会话记录

- **[SESSION_SUMMARY.md](SESSION_SUMMARY.md)** - 📝 本次会话总结
  - 完成的工作
  - 技术进展
  - 遇到的挑战
  - 下一步计划

---

## 🎯 当前状态

### ✅ 已完成

1. **内核加载** - 成功加载19MB内核
2. **高半部映射** - 3级页表配置正确
3. **中断启用** - RFLAGS.IF修复（关键突破！）
4. **内核执行** - 内核持续运行，无崩溃
5. **调试工具** - 循环地址分析代码

### 🔄 进行中

**内核在循环中执行**
- 循环地址：0xffffffff8102200e ↔ 0xffffffff81022010
- 约40 exits/秒
- 中断正常注入

### ⏳ 待完成

1. **突破循环** - 确定原因并提供响应
2. **串口输出** - 看到内核console输出
3. **完整启动** - 进入init进程
4. **运行nginx** - 最终目标

---

## 🚀 快速开始

### 1. 构建

```bash
# 构建libkrunfw（包含内核）
cd src/libkrunfw-win
cargo build --release
cd ../..

# 构建测试
cargo build --release --example test_kernel_boot
```

### 2. 运行

```powershell
# 使用PowerShell脚本（推荐）
.\run_test.ps1

# 或手动运行
$env:RUST_LOG = "info"
.\target\release\examples\test_kernel_boot.exe
```

### 3. 分析

```bash
# 查看循环分析
grep "LOOP #" test_output.log

# 查看进度
grep "progress" test_output.log
```

---

## 📈 项目统计

### 代码

- **核心修改:** 1个文件（whpx_vcpu.rs）
- **新增代码:** 约20行（循环分析）
- **关键修复:** RFLAGS.IF标志（1行，影响巨大！）

### 文档

- **文档数量:** 7个主要文档
- **总行数:** 1500+ 行
- **总字数:** 16000+ 字
- **语言:** 中文为主，技术术语英文

### Git提交

- **提交数:** 8个（本次会话）
- **文件修改:** 9个
- **新增文档:** 5个

---

## 🔑 关键技术

### 虚拟化

- **平台:** Windows Hypervisor Platform (WHPX)
- **API:** WHvRunVirtualProcessor, WHvRequestInterrupt
- **架构:** x86_64 long mode

### 内核

- **类型:** Linux kernel (from libkrunfw)
- **大小:** 19MB
- **映射:** Higher-half (0xffffffff80000000+)

### 中断

- **定时器:** PIT (100 Hz)
- **中断控制器:** IOAPIC + LAPIC stub
- **关键修复:** RFLAGS.IF = 1

---

## 🛠️ 工具和脚本

### PowerShell脚本

- **run_test.ps1** - 自动化测试脚本
  - 设置环境变量
  - 运行测试（5秒超时）
  - 保存日志
  - 提取关键信息

### 日志分析

```bash
# 循环地址
grep "LOOP #" test_output.log | head -20

# vCPU进度
grep "progress" test_output.log

# 定时器
grep "PIT timer" test_output.log

# MMIO访问
grep "MMIO access" test_output.log
```

---

## 📖 推荐阅读顺序

### 新手

1. **DEBUGGING_GUIDE.md** - 了解如何运行测试
2. **PROJECT_SUMMARY.md** - 理解项目全貌
3. **BREAKTHROUGH.md** - 了解关键突破

### 开发者

1. **DEBUG_FINDINGS.md** - 深入技术细节
2. **LOOP_ANALYSIS.md** - 理解分析工具
3. **源代码** - whpx_vcpu.rs, vstate.rs

### 贡献者

1. **SESSION_SUMMARY.md** - 了解最新进展
2. **Git历史** - `git log --oneline`
3. **所有文档** - 全面了解项目

---

## 🤝 贡献

欢迎贡献！当前最需要的帮助：

1. **运行测试** - 收集循环地址数据
2. **分析结果** - 确定循环原因
3. **实现响应** - 突破循环
4. **文档改进** - 补充和完善

---

## 📞 获取帮助

遇到问题？

1. 查看 **DEBUGGING_GUIDE.md** 的常见问题部分
2. 检查 **DEBUG_FINDINGS.md** 的技术细节
3. 查看git提交历史了解最新变化
4. 使用TRACE日志获取更多信息

---

## 📜 许可证

本项目遵循libkrun的许可证（Apache 2.0）。

---

## 🎉 致谢

感谢所有为这个项目做出贡献的人！

特别感谢：
- libkrun项目提供的基础代码
- Windows Hypervisor Platform API文档
- 所有测试和反馈的用户

---

**最后更新:** 2026-03-18
**项目状态:** 🔄 活跃开发中
**下一个里程碑:** 突破内核循环
