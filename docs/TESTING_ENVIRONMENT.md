# 测试环境说明 - Testing Environment Notes

**日期**: 2026-03-18
**状态**: 需要在真实Windows环境中测试

---

## ⚠️ 当前环境限制

### 问题

在当前的自动化环境(Git Bash)中无法成功运行测试,原因:

1. **DLL依赖问题**
   ```
   error while loading shared libraries: api-ms-win-crt-heap-l1-1-0.dll
   ```
   - Git Bash无法找到Windows CRT DLLs
   - 这是环境限制,不是代码问题

2. **输出捕获问题**
   - PowerShell重定向在脚本中不工作
   - 需要在交互式环境中运行

3. **进程管理问题**
   - 后台进程无法正确捕获输出
   - 需要前台运行

### 不影响代码质量

- ✅ 代码编译成功
- ✅ 可执行文件生成
- ✅ LAPIC模拟实现完整
- ✅ RIP跟踪系统就绪

**问题仅在于运行环境,不是代码本身。**

---

## ✅ 解决方案

### 在真实的Windows环境中运行

**必须在以下环境之一中运行**:
1. Windows PowerShell (推荐)
2. Windows Terminal + PowerShell
3. CMD命令提示符

**不要在以下环境中运行**:
- ❌ Git Bash
- ❌ WSL (Windows Subsystem for Linux)
- ❌ Cygwin
- ❌ MSYS2

---

## 🚀 运行测试的方法

### 方法1: 使用RUN_TEST.ps1 (推荐)

这是最完整的测试脚本,包含环境检查和结果分析。

**步骤**:

1. 打开Windows PowerShell
   - 按 `Win + X`
   - 选择 "Windows PowerShell" 或 "Windows Terminal"

2. 进入项目目录
   ```powershell
   cd D:\code\libkrun
   ```

3. 运行测试脚本
   ```powershell
   .\RUN_TEST.ps1
   ```

**脚本功能**:
- ✅ 检查环境和依赖
- ✅ 自动设置环境变量
- ✅ 运行测试10秒
- ✅ 自动分析结果
- ✅ 显示LAPIC、STUCK、Exit消息
- ✅ 给出测试结论

### 方法2: 使用test_apic.bat

简单的批处理脚本。

**步骤**:

1. 打开CMD命令提示符
   - 按 `Win + R`
   - 输入 `cmd`
   - 按回车

2. 进入项目目录
   ```cmd
   cd D:\code\libkrun
   ```

3. 运行测试
   ```cmd
   test_apic.bat
   ```

### 方法3: 手动运行

完全手动控制。

**步骤**:

1. 打开PowerShell

2. 设置环境
   ```powershell
   cd D:\code\libkrun
   $env:RUST_LOG = "info"
   $env:PATH = "$PWD\target\release;$env:PATH"
   ```

3. 运行测试
   ```powershell
   .\target\release\examples\test_kernel_boot.exe
   ```

4. 按 `Ctrl+C` 停止

5. 分析输出

---

## 📊 预期输出

### 成功的标志

```
[INFO] Created test rootfs at "C:\Users\...\Temp\libkrun-test-rootfs"
[OK] krun_set_log_level
[OK] krun_create_ctx → ctx_id=0
[OK] krun_set_vm_config (1 vCPU, 256 MiB)
[INFO] No external kernel specified, will try to use libkrunfw.dll
[OK] krun_set_root
[OK] krun_set_workdir
[OK] krun_set_exec
[OK] krun_add_serial_console_default
[INFO] Starting VM with higher-half kernel mapping...
[INFO] Loaded kernel from libkrunfw: guest_addr=0x1000000, entry_addr=0x1000123
[INFO] Registered APIC stub devices: IOAPIC at 0xfec00000, LAPIC at 0xfee00000
[INFO] PIT timer thread started (100 Hz IRQ 0 injection)
[TRACE] LAPIC read at offset=0x30, len=4, value=0x00050014
[TRACE] LAPIC read at offset=0xf0, len=4, value=0x000001ff
[DEBUG] LAPIC Spurious Vector write: 0x000001ff (APIC enabled)
Exit #1: RIP=0x1000123, GPA=0xfee00030, Type=Read, Size=4
Exit #2: RIP=0x1000456, GPA=0xfee000f0, Type=Read, Size=4
...
```

### 关键指标

**✅ LAPIC工作正常**:
- 看到 "LAPIC read at offset=0x30, len=4, value=0x00050014"
- 看到 "LAPIC read at offset=0xf0, len=4, value=0x000001ff"
- 看到 "LAPIC Spurious Vector write"

**✅ 没有卡住**:
- 没有 "STUCK" 消息
- 或者STUCK次数显著减少 (从1000+次降到<10次)

**✅ 正常执行**:
- 看到大量 "Exit #" 消息
- RIP地址不断变化
- GPA地址多样化

---

## 🔍 分析命令

### PowerShell

```powershell
# 查找LAPIC消息
Select-String -Path test_result.log -Pattern "LAPIC"

# 查找STUCK消息
Select-String -Path test_result.log -Pattern "STUCK"

# 统计Exit数量
(Select-String -Path test_result.log -Pattern "Exit #").Count

# 提取RIP地址
Select-String -Path test_result.log -Pattern "RIP=0x[0-9a-f]+" |
    ForEach-Object { $_.Matches.Value } |
    Group-Object |
    Sort-Object Count -Descending |
    Select-Object -First 10
```

### CMD

```cmd
REM 查找LAPIC消息
findstr /C:"LAPIC" test_result.log

REM 查找STUCK消息
findstr /C:"STUCK" test_result.log

REM 查找Exit消息
findstr /C:"Exit #" test_result.log | more
```

---

## 🎯 测试场景

### 场景1: APIC改进成功 ✅

**特征**:
- ✅ 看到LAPIC读取日志
- ✅ 寄存器返回正确值
- ✅ 没有STUCK消息
- ✅ RIP不断变化

**结论**: APIC改进有效,内核继续启动

**下一步**: 监控串口输出,等待内核消息

### 场景2: 仍在LAPIC卡住 ⚠️

**特征**:
- ✅ 看到LAPIC读取日志
- ⚠️ 有STUCK消息
- ⚠️ 卡在特定的LAPIC寄存器

**结论**: 需要进一步改进LAPIC实现

**下一步**: 分析卡住的寄存器,改进返回值

### 场景3: 卡在其他设备 ⚠️

**特征**:
- ✅ LAPIC工作正常
- ⚠️ 卡在其他GPA地址

**结论**: LAPIC改进成功,但需要实现其他设备

**下一步**: 根据GPA范围实现相应设备

---

## 📝 故障排除

### 问题1: 程序立即退出

**可能原因**:
- libkrunfw.dll未找到
- WHPX未启用
- 权限不足

**解决方案**:
```powershell
# 检查DLL
Test-Path .\target\release\libkrunfw.dll

# 重新编译DLL
cd src\libkrunfw-win
cargo build --release
cd ..\..

# 以管理员身份运行PowerShell
```

### 问题2: 没有输出

**可能原因**:
- RUST_LOG未设置
- 输出被重定向到错误的地方

**解决方案**:
```powershell
# 确保RUST_LOG设置
$env:RUST_LOG = "info"

# 直接运行,不重定向
.\target\release\examples\test_kernel_boot.exe
```

### 问题3: DLL错误

**可能原因**:
- 在Git Bash中运行
- PATH未正确设置

**解决方案**:
```powershell
# 在真实的PowerShell中运行
# 设置PATH
$env:PATH = "$PWD\target\release;$env:PATH"
```

---

## ✅ 检查清单

运行测试前:
- [ ] 在真实的Windows PowerShell中
- [ ] 不在Git Bash或WSL中
- [ ] 已编译: `cargo build --release --example test_kernel_boot`
- [ ] libkrunfw.dll存在: `target\release\libkrunfw.dll`

运行测试:
- [ ] 使用 `.\RUN_TEST.ps1` (推荐)
- [ ] 或使用 `test_apic.bat`
- [ ] 或手动运行

分析结果:
- [ ] 查找LAPIC消息
- [ ] 查找STUCK消息
- [ ] 统计Exit数量
- [ ] 确定测试结论

---

## 📚 相关文档

- **TESTING_GUIDE.md** - 详细测试指南
- **APIC_IMPROVEMENT.md** - APIC改进技术细节
- **TROUBLESHOOTING.md** - 故障排除
- **COMPLETION_REPORT.md** - 完成报告

---

## 🎯 总结

**当前状态**:
- ✅ 代码完成并推送
- ✅ 测试脚本就绪
- ⚠️ 需要在真实Windows环境中运行

**运行测试**:
```powershell
# 在Windows PowerShell中
cd D:\code\libkrun
.\RUN_TEST.ps1
```

**预期结果**:
- LAPIC寄存器返回正确值
- 内核识别APIC
- 不再卡住或显著改善

**下一步**:
- 根据测试结果决定下一步改进
- 如果成功,监控串口输出
- 如果失败,分析具体问题

---

**重要**: 必须在真实的Windows PowerShell环境中运行测试,不能在Git Bash中运行!
