# Windows WHPX 内核启动调试指南

本指南说明如何使用最新的循环分析工具来调试Windows WHPX上的Linux内核启动。

---

## 快速开始

### 1. 构建项目

```bash
# 构建libkrunfw（包含内核）
cd src/libkrunfw-win
cargo build --release
cd ../..

# 构建测试示例
cargo build --release --example test_kernel_boot
```

### 2. 运行测试

**方法A: 使用PowerShell脚本（推荐）**

```powershell
# 在PowerShell中运行
.\run_test.ps1
```

**方法B: 手动运行**

```powershell
# 设置日志级别
$env:RUST_LOG = "info"

# 运行测试（5秒超时）
.\target\release\examples\test_kernel_boot.exe
```

**方法C: 使用cargo**

```bash
# 在Windows命令提示符或PowerShell中
set RUST_LOG=info
cargo run --release --example test_kernel_boot
```

### 3. 查看结果

测试运行后，检查输出中的关键信息：

```bash
# 查看循环地址分析
grep "LOOP #" test_output.log

# 查看vCPU进度
grep "progress" test_output.log

# 查看定时器中断
grep "PIT timer" test_output.log
```

---

## 日志级别

不同的日志级别提供不同详细程度的信息：

### TRACE - 最详细

```bash
$env:RUST_LOG = "trace"
```

**输出内容:**
- 每个VM exit的详细信息
- RIP（指令指针）变化
- 所有MMIO/IO访问

**用途:** 深入分析VM exit模式

### INFO - 推荐

```bash
$env:RUST_LOG = "info"
```

**输出内容:**
- 循环地址分析（前10次 + 每100次）
- vCPU进度（每5秒）
- 定时器中断统计（每100次）
- 关键事件

**用途:** 日常调试和分析

### DEBUG - 中等详细

```bash
$env:RUST_LOG = "debug"
```

**输出内容:**
- MMIO访问详情
- 指令字节
- 设备I/O

**用途:** 调试设备模拟

---

## 理解输出

### 循环地址分析

```
[INFO] 🔍 LOOP #1: RIP=0xffffffff8102200e, GPA=0x12345678, Type=Read, Size=8
```

**字段说明:**
- `LOOP #1` - 循环计数器
- `RIP=0xffffffff8102200e` - 指令指针（虚拟地址）
- `GPA=0x12345678` - 物理地址
- `Type=Read` - 访问类型（Read/Write/Execute）
- `Size=8` - 访问大小（字节）

**分析方法:**

1. **检查GPA范围**
   - `0x0 - 0x40000000` → 普通内存
   - `0xfec00000` → IOAPIC寄存器
   - `0xfee00000` → LAPIC寄存器

2. **检查访问类型**
   - `Read` → 可能是轮询或读取锁状态
   - `Write` → 可能是更新状态或释放锁
   - `Execute` → 不太可能（应该是数据访问）

3. **检查GPA是否相同**
   - 相同 → 确认是轮询同一位置
   - 不同 → 访问不同的数据结构

### vCPU进度

```
[INFO] vCPU 0 progress: 200 exits processed
```

**含义:**
- vCPU线程正在运行
- 已处理200个VM exit
- 每5秒报告一次

**如果没有进度日志:**
- vCPU线程可能卡住
- 或者没有VM exit发生

### 定时器中断

```
[DEBUG] PIT timer: injected 100 IRQ 0 interrupts
```

**含义:**
- 定时器线程正在工作
- 已注入100个中断
- 每100次报告一次

---

## 常见问题

### Q: 运行时出现"DLL not found"错误

**A:** 这通常发生在Git Bash环境中。解决方法：

1. 使用PowerShell或Windows命令提示符
2. 或者在Git Bash中设置完整的PATH：
   ```bash
   export PATH="/c/Windows/System32:$PATH"
   ```

### Q: 没有看到循环地址日志

**A:** 可能的原因：

1. 日志级别不够：确保使用`RUST_LOG=info`或更高
2. 内核还没到达循环地址：等待更长时间
3. 循环地址已改变：检查TRACE日志找到新的循环地址

### Q: vCPU进度日志不出现

**A:** 可能的原因：

1. vCPU线程没有启动：检查是否有错误日志
2. 没有VM exit：内核可能在HLT状态
3. 时间不够：至少运行5秒

### Q: 如何保存日志到文件

**A:** 使用重定向：

```powershell
# PowerShell
.\target\release\examples\test_kernel_boot.exe > output.log 2>&1

# 或使用脚本
.\run_test.ps1  # 自动保存到test_output.log
```

---

## 下一步分析

### 1. 确定循环原因

根据循环地址日志，判断：

- **如果GPA相同且Type=Read** → 可能是自旋锁或轮询
- **如果GPA在APIC范围** → 可能在等待中断控制器
- **如果GPA在普通内存** → 可能在等待共享变量

### 2. 反汇编循环代码

```bash
# 从guest内存读取指令（需要添加代码）
# 然后使用objdump反汇编
objdump -D -b binary -m i386:x86-64 loop_instructions.bin
```

### 3. 提供解决方案

根据分析结果：

- **自旋锁** → 检查是否需要其他CPU核心或中断
- **轮询设备** → 实现设备响应
- **等待中断** → 检查中断传递路径

---

## 相关文档

- **BREAKTHROUGH.md** - 重大突破记录
- **DEBUG_FINDINGS.md** - 详细调试发现
- **LOOP_ANALYSIS.md** - 循环分析工具说明
- **PROJECT_SUMMARY.md** - 项目完整总结

---

## 获取帮助

如果遇到问题：

1. 检查相关文档
2. 查看git提交历史：`git log --oneline`
3. 检查最近的修改：`git diff HEAD~5`
4. 查看TRACE日志获取更多细节

---

## 贡献

欢迎提交改进建议和bug报告！

当前重点：
- 确定循环地址的行为
- 实现相应的设备响应
- 让内核继续启动
