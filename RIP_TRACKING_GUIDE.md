# RIP Tracking Guide - 内核执行跟踪指南

## 概述 (Overview)

已在 `src/vmm/src/windows/whpx_vcpu.rs` 中添加了全面的RIP跟踪代码,用于诊断内核卡住的问题。

## 添加的跟踪功能 (Added Tracking Features)

### 1. 通用MemoryAccess跟踪

代码位置: `whpx_vcpu.rs:1047-1088`

跟踪所有MemoryAccess VM exits,记录:
- **RIP** (指令指针): 当前执行的虚拟地址
- **GPA** (Guest Physical Address): 访问的物理地址
- **Access Type**: Read(0) / Write(1) / Execute(2)
- **Access Size**: 访问大小(字节)

### 2. 卡住检测 (Stuck Detection)

自动检测内核是否卡在同一个RIP地址:

```rust
static mut LAST_RIP: u64 = 0;
static mut SAME_RIP_COUNT: u64 = 0;
```

**检测逻辑:**
- 如果RIP连续100次相同 → 输出 `⚠️ STUCK` 警告
- 每1000次重复 → 输出进度更新
- RIP改变时 → 如果之前卡住≥100次,输出 `✓ Unstuck` 消息

### 3. 输出示例

**正常执行:**
```
Exit #1: RIP=0x1000123, GPA=0xfee00000, Type=Read, Size=4
Exit #2: RIP=0x1000127, GPA=0xfec00000, Type=Write, Size=8
...
Exit #1000: RIP=0xffffffff81022010, GPA=0x7ff8, Type=Read, Size=8
```

**检测到卡住:**
```
⚠️  STUCK: RIP=0xffffffff8102200e repeated 100 times, GPA=0xfee00030, Type=Read, Size=4
⚠️  STUCK: RIP=0xffffffff8102200e repeated 1000 times
⚠️  STUCK: RIP=0xffffffff8102200e repeated 2000 times
✓ Unstuck from RIP=0xffffffff8102200e after 2543 exits, now at 0xffffffff81022100
```

## 如何运行测试 (How to Run Tests)

### 方法1: PowerShell (推荐)

```powershell
# 1. 打开PowerShell (不是Git Bash)
# 2. 进入项目目录
cd D:\code\libkrun

# 3. 设置环境变量
$env:RUST_LOG = "info"
$env:PATH = "$PWD\target\release;$env:PATH"

# 4. 编译
cargo build --release --example test_kernel_boot

# 5. 运行测试 (10秒后手动Ctrl+C停止)
.\target\release\examples\test_kernel_boot.exe 2>&1 | Tee-Object test_rip_tracking.log

# 6. 分析输出
Select-String -Path test_rip_tracking.log -Pattern "STUCK|Exit #" | Select-Object -First 50
```

### 方法2: 使用提供的脚本

```powershell
.\run_test_proper.ps1
```

### 方法3: 批处理文件

```cmd
run_test.bat
```

## 分析输出 (Analyzing Output)

### 查找卡住的地址

```bash
grep "STUCK" test_rip_tracking.log
```

### 查看前20个exits

```bash
grep "Exit #" test_rip_tracking.log | head -20
```

### 统计exit总数

```bash
grep -c "Exit #" test_rip_tracking.log
```

### 提取唯一的RIP地址

```bash
grep "Exit #" test_rip_tracking.log | sed 's/.*RIP=\(0x[0-9a-f]*\).*/\1/' | sort | uniq -c | sort -rn
```

## 预期结果 (Expected Results)

根据之前的观察,内核可能会:

1. **卡在APIC轮询** (最可能)
   ```
   STUCK: RIP=0xffffffff8102200e, GPA=0xfee00030, Type=Read, Size=4
   ```
   - GPA在LAPIC范围 (0xfee00000-0xfee00fff)
   - 访问类型: Read
   - 说明: 内核在轮询LAPIC寄存器等待中断

2. **卡在自旋锁**
   ```
   STUCK: RIP=0xffffffff8102200e, GPA=0x12345678, Type=Read, Size=8
   ```
   - GPA在正常内存范围
   - 访问类型: Read
   - 说明: 内核在等待锁释放

3. **卡在I/O端口**
   ```
   STUCK: RIP=0xffffffff8102200e, GPA=0x3f8, Type=Read, Size=1
   ```
   - GPA在I/O端口范围 (< 0x10000)
   - 说明: 内核在等待串口或其他设备

## 下一步行动 (Next Steps)

根据跟踪结果:

### 如果卡在LAPIC (0xfee00000-0xfee00fff)

**问题**: 内核在等待LAPIC中断,但我们的APIC stub没有正确响应

**解决方案**:
1. 检查 `src/vmm/src/builder.rs` 中的APIC stub实现
2. 确保LAPIC寄存器返回合理的值
3. 可能需要实现更完整的LAPIC模拟

### 如果卡在正常内存

**问题**: 可能是自旋锁或等待共享变量

**解决方案**:
1. 检查是否需要注入中断来唤醒内核
2. 检查PIT定时器是否正常工作
3. 可能需要实现SMP相关的同步机制

### 如果卡在I/O端口

**问题**: 内核在等待设备响应

**解决方案**:
1. 实现相应的设备模拟
2. 或者修改内核配置跳过该设备

## 代码位置 (Code Locations)

- **RIP跟踪代码**: `src/vmm/src/windows/whpx_vcpu.rs:1047-1088`
- **APIC stub**: `src/vmm/src/builder.rs:1050-1100`
- **PIT定时器**: `src/vmm/src/builder.rs:1102-1150`
- **测试程序**: `examples/test_kernel_boot.rs`

## 故障排除 (Troubleshooting)

### 问题: 没有任何输出

**可能原因**:
1. RUST_LOG环境变量未设置
2. 程序没有启动(DLL缺失)
3. 输出被重定向到错误的地方

**解决方案**:
```powershell
# 确保RUST_LOG设置
$env:RUST_LOG = "info"

# 确保DLL在PATH中
$env:PATH = "$PWD\target\release;$env:PATH"

# 直接运行,不重定向
.\target\release\examples\test_kernel_boot.exe
```

### 问题: DLL not found错误

**解决方案**:
```powershell
# 重新编译libkrunfw
cd src\libkrunfw-win
cargo build --release
cd ..\..

# 确保DLL在正确位置
Copy-Item src\libkrunfw-win\target\release\libkrunfw.dll target\release\
```

### 问题: 程序立即退出

**可能原因**: 内核启动失败

**解决方案**:
1. 检查内核是否正确加载
2. 查看是否有错误消息
3. 确保页表配置正确

## 总结 (Summary)

新的RIP跟踪代码提供了:
- ✅ 自动检测卡住的RIP地址
- ✅ 记录GPA、访问类型和大小
- ✅ 统计重复次数
- ✅ 检测何时脱离卡住状态

这些信息足以诊断内核为什么卡住,以及需要实现什么功能来让它继续执行。

**关键指标**:
- 如果看到 `STUCK` 消息 → 内核确实卡住了
- 检查GPA范围 → 确定是APIC、内存还是I/O
- 检查访问类型 → 确定是读(轮询)还是写
- 检查重复次数 → 评估问题严重程度

运行测试后,将输出发送给我进行详细分析!
