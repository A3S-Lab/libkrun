# Windows WHPX 快速参考

> 一页纸速查手册

---

## 🚀 快速命令

### 构建

```bash
cd src/libkrunfw-win && cargo build --release && cd ../..
cargo build --release --example test_kernel_boot
```

### 运行

```powershell
# PowerShell
.\run_test.ps1

# 或
$env:RUST_LOG="info"
.\target\release\examples\test_kernel_boot.exe
```

### 分析

```bash
grep "LOOP #" test_output.log | head -10    # 循环分析
grep "progress" test_output.log              # vCPU进度
grep "PIT timer" test_output.log             # 定时器
```

---

## 📊 当前状态

| 项目 | 状态 | 说明 |
|------|------|------|
| 内核加载 | ✅ | 19MB, 入口0x1000123 |
| 页表配置 | ✅ | 3级, higher-half映射 |
| 中断启用 | ✅ | RFLAGS.IF=1 |
| 内核执行 | ✅ | 持续运行 |
| 循环状态 | 🔄 | 0xffffffff8102200e ↔ 0xffffffff81022010 |
| 串口输出 | ⏳ | 待实现 |

---

## 🔑 关键代码

### RFLAGS.IF 修复
**文件:** `src/vmm/src/windows/vstate.rs:372`
```rust
v[4].Reg64 = 0x2 | (1 << 9);  // 启用中断
```

### 循环分析
**文件:** `src/vmm/src/windows/whpx_vcpu.rs:1050`
```rust
if rip == 0xffffffff8102200e || rip == 0xffffffff81022010 {
    info!("🔍 LOOP #{}: RIP={:#x}, GPA={:#x}, Type={}, Size={}",
          count, rip, gpa, access_type, access_size);
}
```

---

## 📚 文档速查

| 文档 | 用途 | 何时查看 |
|------|------|----------|
| [DEBUGGING_GUIDE.md](DEBUGGING_GUIDE.md) | 调试指南 | 🚀 开始使用 |
| [DOCS_INDEX.md](DOCS_INDEX.md) | 文档索引 | 📖 查找文档 |
| [BREAKTHROUGH.md](BREAKTHROUGH.md) | 突破记录 | 🎉 了解进展 |
| [DEBUG_FINDINGS.md](DEBUG_FINDINGS.md) | 技术细节 | 🔍 深入分析 |
| [PROJECT_SUMMARY.md](PROJECT_SUMMARY.md) | 项目总结 | 📊 全面了解 |

---

## 🔍 日志级别

| 级别 | 用途 | 输出内容 |
|------|------|----------|
| `TRACE` | 深入分析 | 所有VM exit |
| `INFO` | 日常调试 | 循环+进度+关键事件 |
| `DEBUG` | 设备调试 | MMIO访问+设备I/O |

---

## 🐛 常见问题

### DLL not found
**解决:** 使用PowerShell而非Git Bash

### 没有循环日志
**检查:**
1. `RUST_LOG=info`是否设置
2. 运行时间是否足够（>2秒）

### vCPU无进度
**检查:**
1. 是否有错误日志
2. 运行时间是否>5秒

---

## 📈 性能指标

| 指标 | 当前值 | 目标值 |
|------|--------|--------|
| VM exit频率 | ~40/秒 | - |
| 中断注入 | 100 Hz | 100 Hz |
| 启动时间 | - | <5秒 |

---

## 🎯 下一步

1. **运行测试** → 收集循环数据
2. **分析GPA** → 确定访问目标
3. **实现响应** → 突破循环
4. **串口输出** → 看到console

---

## 🔗 快速链接

- **开始使用:** [DEBUGGING_GUIDE.md](DEBUGGING_GUIDE.md)
- **查看进展:** [BREAKTHROUGH.md](BREAKTHROUGH.md)
- **技术细节:** [DEBUG_FINDINGS.md](DEBUG_FINDINGS.md)
- **完整总结:** [PROJECT_SUMMARY.md](PROJECT_SUMMARY.md)

---

## 💡 提示

- 使用PowerShell运行测试
- INFO日志级别最实用
- 前10次循环全记录
- 每100次循环记录一次
- 每5秒报告vCPU进度

---

**最后更新:** 2026-03-18
**打印友好** | **一页速查**
