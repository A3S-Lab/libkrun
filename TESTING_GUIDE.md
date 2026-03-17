# 内核启动测试和分析指南 - Kernel Boot Testing and Analysis Guide

**更新时间**: 2026-03-18 06:00

---

## 🎯 当前状态

### 已实现的功能

✅ **RIP跟踪系统**
- 自动检测卡住(100+次重复)
- 记录GPA、访问类型、大小
- 智能日志输出

✅ **LAPIC寄存器模拟**
- LAPIC_VERSION: 0x00050014
- LAPIC_SPURIOUS: 0x1FF (enabled)
- ISR/IRR/TMR: 0x00 (no interrupts)
- TPR/EOI: 状态跟踪

✅ **中断注入**
- PIT定时器: 100 Hz IRQ 0
- WHvRequestInterrupt API
- 中断向量映射: IRQ → 0x20+IRQ

✅ **串口设备**
- COM1 (0x3f8) 自动配置
- stdout输出
- stdin输入

---

## 🧪 测试方法

### 方法1: 使用批处理脚本 (推荐)

```cmd
test_apic.bat
```

这个脚本会:
1. 设置环境变量
2. 运行测试
3. 捕获输出到 test_apic.log
4. 自动分析关键模式

### 方法2: 手动运行

```cmd
set RUST_LOG=info
set PATH=%CD%\target\release;%PATH%
target\release\examples\test_kernel_boot.exe > test_manual.log 2>&1
```

按Ctrl+C停止后分析日志。

### 方法3: PowerShell

```powershell
$env:RUST_LOG = "info"
$env:PATH = "$PWD\target\release;$env:PATH"
.\target\release\examples\test_kernel_boot.exe 2>&1 | Tee-Object test_ps.log
```

---

## 📊 预期输出分析

### 场景1: APIC改进成功 ✅

**特征**:
```
[INFO] Registered APIC stub devices: IOAPIC at 0xfec00000, LAPIC at 0xfee00000
[INFO] PIT timer thread started (100 Hz IRQ 0 injection)
[TRACE] LAPIC read at offset=0x30, len=4, value=0x00050014
[TRACE] LAPIC read at offset=0xf0, len=4, value=0x000001ff
[DEBUG] LAPIC Spurious Vector write: 0x000001ff (APIC enabled)
Exit #1: RIP=0x1000123, GPA=0xfee00030, Type=Read, Size=4
Exit #2: RIP=0x1000456, GPA=0xfee000f0, Type=Read, Size=4
Exit #3: RIP=0x1000789, GPA=0x7ff8, Type=Write, Size=8
...
(RIP不断变化,没有STUCK消息)
```

**分析**:
- ✅ 内核读取LAPIC寄存器
- ✅ 获得正确的值
- ✅ 配置APIC
- ✅ 继续执行

**下一步**: 监控串口输出,等待内核消息

### 场景2: 仍在LAPIC卡住 ⚠️

**特征**:
```
[TRACE] LAPIC read at offset=0x30, len=4, value=0x00050014
⚠️ STUCK: RIP=0xffffffff8102200e repeated 100 times, GPA=0xfee00030, Type=Read, Size=4
⚠️ STUCK: RIP=0xffffffff8102200e repeated 1000 times
```

**分析**:
- ⚠️ 内核仍在轮询LAPIC
- ⚠️ 可能在等待特定的寄存器值
- ⚠️ 需要检查具体的偏移量

**下一步**:
1. 记录GPA偏移量 (GPA - 0xfee00000)
2. 查看是在读取哪个寄存器
3. 检查该寄存器的返回值是否正确

### 场景3: 卡在其他设备 ⚠️

**特征**:
```
Exit #1: RIP=0x1000123, GPA=0xfee00030, Type=Read, Size=4
Exit #2: RIP=0x1000456, GPA=0xfee000f0, Type=Read, Size=4
...
⚠️ STUCK: RIP=0xffffffff81234567 repeated 100 times, GPA=0x3f8, Type=Read, Size=1
```

**分析**:
- ✅ LAPIC工作正常
- ⚠️ 卡在串口 (0x3f8)
- ⚠️ 可能在等待串口状态

**下一步**: 检查串口设备实现

### 场景4: 卡在未知MMIO ⚠️

**特征**:
```
⚠️ STUCK: RIP=0xffffffff81234567 repeated 100 times, GPA=0x12345678, Type=Read, Size=4
```

**分析GPA范围**:
- 0xfee00000-0xfee00fff: LAPIC
- 0xfec00000-0xfec00fff: IOAPIC
- 0x0-0xffff: I/O端口
- 0xf0000000-0xffffffff: PCI配置空间
- 其他: 正常内存或未知设备

**下一步**: 根据GPA范围实现相应设备

### 场景5: 成功启动 🎉

**特征**:
```
Exit #1: RIP=0x1000123, GPA=0xfee00030, Type=Read, Size=4
...
Exit #1000: RIP=0xffffffff81234567, GPA=0x3f8, Type=Write, Size=1
...
(大量不同的RIP地址,没有STUCK)
(可能看到串口输出)
```

**分析**:
- ✅ 内核正常执行
- ✅ 访问各种设备
- ✅ 可能已经启动完成

**下一步**: 检查串口输出,尝试交互

---

## 🔍 日志分析命令

### Windows CMD

```cmd
REM 查找LAPIC消息
findstr /C:"LAPIC" test_apic.log

REM 查找STUCK消息
findstr /C:"STUCK" test_apic.log

REM 查找Exit消息
findstr /C:"Exit #" test_apic.log | more

REM 统计Exit数量
findstr /C:"Exit #" test_apic.log | find /C "Exit #"

REM 查找PIT定时器消息
findstr /C:"PIT" test_apic.log
```

### PowerShell

```powershell
# 查找LAPIC消息
Select-String -Path test_apic.log -Pattern "LAPIC"

# 查找STUCK消息
Select-String -Path test_apic.log -Pattern "STUCK"

# 统计Exit数量
(Select-String -Path test_apic.log -Pattern "Exit #").Count

# 提取唯一的RIP地址
Select-String -Path test_apic.log -Pattern "RIP=0x[0-9a-f]+" |
    ForEach-Object { $_.Matches.Value } |
    Group-Object |
    Sort-Object Count -Descending |
    Select-Object -First 10

# 提取唯一的GPA地址
Select-String -Path test_apic.log -Pattern "GPA=0x[0-9a-f]+" |
    ForEach-Object { $_.Matches.Value } |
    Group-Object |
    Sort-Object Count -Descending |
    Select-Object -First 10
```

---

## 📈 性能指标

### 正常执行

- **Exit频率**: 100-1000 exits/秒
- **RIP变化**: 每个exit都不同或变化频繁
- **STUCK次数**: 0次或偶尔<10次

### 卡住状态

- **Exit频率**: 10-100 exits/秒
- **RIP变化**: 同一个RIP重复100+次
- **STUCK次数**: 持续增加

---

## 🐛 调试技巧

### 1. 确定卡住的寄存器

如果卡在LAPIC:
```
STUCK: GPA=0xfee00030
偏移 = 0xfee00030 - 0xfee00000 = 0x30
寄存器 = LAPIC_VERSION (0x30)
```

检查 `windows_apic_stub.rs` 中该寄存器的返回值。

### 2. 检查中断注入

查找PIT定时器日志:
```
[DEBUG] PIT timer: injected 100 IRQ 0 interrupts
[DEBUG] PIT timer: injected 200 IRQ 0 interrupts
```

如果没有这些消息,说明PIT定时器没有运行。

### 3. 检查中断传递

查找中断注入日志:
```
[TRACE] WHvRequestInterrupt succeeded for IRQ 0 (vector 32)
```

如果有错误消息,说明中断注入失败。

### 4. 增加日志级别

```cmd
set RUST_LOG=trace
```

这会输出更详细的日志,包括所有LAPIC读写操作。

---

## 🔧 常见问题

### Q1: 没有任何输出

**可能原因**:
- DLL未找到
- 环境变量未设置
- 输出被重定向到错误的地方

**解决方案**:
```cmd
REM 确保DLL在PATH中
set PATH=%CD%\target\release;%PATH%

REM 确保RUST_LOG设置
set RUST_LOG=info

REM 直接运行,不重定向
target\release\examples\test_kernel_boot.exe
```

### Q2: 程序立即退出

**可能原因**:
- 内核加载失败
- WHPX初始化失败
- 权限问题

**解决方案**:
查看错误消息,检查:
- libkrunfw.dll是否存在
- WHPX是否启用
- 是否有管理员权限

### Q3: 大量MMIO access错误

**可能原因**:
- 访问了未实现的设备
- MMIO地址范围错误

**解决方案**:
记录GPA地址,实现相应的设备stub。

---

## 📝 测试检查清单

运行测试前:
- [ ] 编译成功 (`cargo build --release --example test_kernel_boot`)
- [ ] libkrunfw.dll存在 (`target\release\libkrunfw.dll`)
- [ ] RUST_LOG设置为info或trace
- [ ] PATH包含target\release

运行测试后:
- [ ] 检查是否有输出
- [ ] 查找LAPIC读取消息
- [ ] 查找STUCK消息
- [ ] 统计Exit数量
- [ ] 检查RIP地址变化
- [ ] 查找PIT定时器消息

分析结果:
- [ ] 确定是否卡住
- [ ] 如果卡住,记录GPA和RIP
- [ ] 确定卡住的设备/寄存器
- [ ] 规划下一步改进

---

## 🎯 下一步行动

### 如果APIC工作正常

1. **监控启动进度**
   - 观察RIP地址变化
   - 查找串口输出
   - 等待内核消息

2. **实现其他设备**
   - 根据访问模式实现需要的设备
   - 优先实现高频访问的设备

3. **优化性能**
   - 减少日志输出
   - 优化MMIO处理

### 如果仍然卡住

1. **详细分析**
   - 记录卡住的GPA和偏移
   - 确定是哪个寄存器
   - 检查返回值是否正确

2. **改进实现**
   - 修正寄存器返回值
   - 实现缺失的寄存器
   - 添加状态跟踪

3. **增加日志**
   - 使用RUST_LOG=trace
   - 记录所有MMIO访问
   - 分析访问模式

---

## 📚 相关文档

- **APIC_IMPROVEMENT.md** - APIC改进详解
- **KERNEL_DEV_PROGRESS.md** - 内核开发进展
- **RIP_TRACKING_GUIDE.md** - RIP跟踪指南
- **TROUBLESHOOTING.md** - 故障排除

---

## ✅ 总结

**测试准备**:
- ✅ 代码已编译
- ✅ APIC改进已实现
- ✅ RIP跟踪已启用
- ✅ 测试脚本已创建

**预期结果**:
- 🎯 内核识别LAPIC
- 🎯 不再在LAPIC轮询卡住
- 🎯 启动过程继续进行

**运行测试**:
```cmd
test_apic.bat
```

**分析结果**:
```cmd
findstr /C:"LAPIC" test_apic.log
findstr /C:"STUCK" test_apic.log
findstr /C:"Exit #" test_apic.log | more
```

**下一步**: 根据测试结果决定下一步改进方向!
