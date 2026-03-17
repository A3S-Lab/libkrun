# 故障排除指南

> 常见问题的诊断和解决方案

---

## 🔧 构建问题

### 问题：cargo build失败，提示缺少依赖

**症状:**
```
error: could not compile `cpuid` (lib) due to 6 previous errors
error: unresolved import `kvm_bindings`
```

**原因:** 某些Linux特定的依赖在Windows上不可用

**解决方案:**
```bash
# 只构建Windows需要的部分
cargo build --release --example test_kernel_boot

# 如果需要libkrunfw
cd src/libkrunfw-win
cargo build --release
cd ../..
```

---

### 问题：libkrunfw.dll not found

**症状:**
```
error: process didn't exit successfully (exit code: 0xc0000135, STATUS_DLL_NOT_FOUND)
```

**原因:** cargo clean删除了libkrunfw.dll

**解决方案:**
```bash
# 重新构建libkrunfw
cd src/libkrunfw-win
cargo build --release
cd ../..

# 验证DLL存在
ls target/release/libkrunfw.dll
```

---

## 🏃 运行问题

### 问题：Git Bash中运行出现DLL错误

**症状:**
```
error while loading shared libraries: api-ms-win-crt-heap-l1-1-0.dll
```

**原因:** Git Bash环境缺少Windows CRT DLL路径

**解决方案:**

**方法1: 使用PowerShell（推荐）**
```powershell
.\run_test.ps1
```

**方法2: 使用Windows命令提示符**
```cmd
set RUST_LOG=info
.\target\release\examples\test_kernel_boot.exe
```

**方法3: 在Git Bash中设置PATH**
```bash
export PATH="/c/Windows/System32:$PATH"
./target/release/examples/test_kernel_boot.exe
```

---

### 问题：程序启动后无输出

**症状:**
- 程序运行但没有任何输出
- test_output.log文件为空

**诊断步骤:**

1. **检查日志级别**
```powershell
# 确保设置了RUST_LOG
$env:RUST_LOG
# 应该输出: info 或 debug 或 trace
```

2. **检查进程是否运行**
```powershell
Get-Process test_kernel_boot -ErrorAction SilentlyContinue
```

3. **检查错误日志**
```bash
cat test_error.log
```

**解决方案:**
```powershell
# 重新设置环境变量
$env:RUST_LOG = "info"

# 直接运行并查看输出
.\target\release\examples\test_kernel_boot.exe 2>&1 | Tee-Object -FilePath output.log
```

---

## 📊 日志问题

### 问题：没有看到循环地址日志

**症状:**
```bash
grep "LOOP #" test_output.log
# 无输出
```

**可能原因:**

1. **日志级别不够**
```bash
# 检查
echo $RUST_LOG
# 应该是 info 或更高
```

2. **内核还没到达循环地址**
```bash
# 检查是否有其他日志
grep "vCPU.*starting" test_output.log
grep "Kernel loaded" test_output.log
```

3. **运行时间不够**
```bash
# 至少运行2-3秒
timeout 5 ./target/release/examples/test_kernel_boot.exe
```

**解决方案:**
```powershell
# 使用TRACE级别查看所有VM exit
$env:RUST_LOG = "trace"
.\target\release\examples\test_kernel_boot.exe 2>&1 | Select-String "WHPX exit" | Select-Object -First 20
```

---

### 问题：vCPU进度日志不出现

**症状:**
```bash
grep "progress" test_output.log
# 无输出
```

**可能原因:**

1. **vCPU线程未启动**
```bash
# 检查是否有配置日志
grep "Configuring vCPU" test_output.log
```

2. **没有VM exit发生**
```bash
# 检查TRACE日志
RUST_LOG=trace timeout 3 ./test_kernel_boot.exe 2>&1 | grep "WHPX exit" | wc -l
# 应该 > 0
```

3. **运行时间不够**
```bash
# 进度日志每5秒报告一次
timeout 10 ./test_kernel_boot.exe
```

**解决方案:**
```bash
# 运行更长时间
timeout 10 cargo run --release --example test_kernel_boot 2>&1 | tee long_run.log
grep "progress" long_run.log
```

---

## 🔍 分析问题

### 问题：无法确定循环原因

**症状:**
- 看到循环日志但不知道含义

**分析步骤:**

1. **检查GPA范围**
```bash
grep "LOOP #" test_output.log | head -10
```

查看GPA值：
- `0x0 - 0x40000000` → 普通内存
- `0xfec00000` → IOAPIC
- `0xfee00000` → LAPIC
- 其他 → 未映射区域

2. **检查访问类型**
```bash
grep "Type=Read" test_output.log | wc -l   # 读取次数
grep "Type=Write" test_output.log | wc -l  # 写入次数
```

3. **检查访问模式**
```bash
# 是否总是访问同一个GPA？
grep "LOOP #" test_output.log | awk '{print $6}' | sort | uniq -c
```

**解决方案:**

根据分析结果：
- **GPA相同 + Type=Read** → 可能是自旋锁或轮询
- **GPA在APIC范围** → 等待中断控制器
- **GPA在普通内存** → 等待共享变量

---

## 🐛 内核问题

### 问题：内核panic或崩溃

**症状:**
```
[WARN] vCPU 0 shutdown - VM terminated abnormally
```

**诊断步骤:**

1. **检查最后的RIP**
```bash
grep "WHPX exit" trace.log | tail -5
```

2. **检查是否有异常**
```bash
grep "exception\|Exception" debug.log
```

3. **检查页表配置**
```bash
grep "Page tables configured" info.log
```

**解决方案:**

查看 [DEBUG_FINDINGS.md](DEBUG_FINDINGS.md) 的技术细节部分

---

### 问题：内核进入HLT后不响应

**症状:**
```
[WARN] vCPU 0 halted - kernel may have failed to boot
```

**可能原因:**

1. **中断未启用**
```bash
# 检查RFLAGS配置
grep "RFLAGS" src/vmm/src/windows/vstate.rs
# 应该看到: v[4].Reg64 = 0x2 | (1 << 9);
```

2. **中断未注入**
```bash
# 检查定时器日志
grep "PIT timer" debug.log
```

3. **中断未传递**
```bash
# 检查WHvRequestInterrupt
grep "WHvRequestInterrupt" trace.log
```

**解决方案:**

确保RFLAGS.IF已设置（见 [BREAKTHROUGH.md](BREAKTHROUGH.md)）

---

## 🔄 循环问题

### 问题：内核一直在循环，无法继续

**症状:**
- 循环日志持续出现
- RIP在两个地址间跳转
- 无串口输出

**当前状态:** 这是已知问题，正在分析中

**下一步:**

1. **收集循环数据**
```bash
grep "LOOP #" test_output.log > loop_data.txt
```

2. **分析访问模式**
```bash
# 查看前20次循环
head -20 loop_data.txt

# 统计访问类型
grep "Type=" loop_data.txt | cut -d',' -f3 | sort | uniq -c
```

3. **查看文档**
- [LOOP_ANALYSIS.md](LOOP_ANALYSIS.md) - 分析方法
- [DEBUG_FINDINGS.md](DEBUG_FINDINGS.md) - 技术细节

---

## 📝 日志收集

### 完整诊断日志收集

```powershell
# PowerShell脚本
$env:RUST_LOG = "trace"
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$logfile = "diagnostic_$timestamp.log"

Write-Host "Collecting diagnostic logs to $logfile"

# 系统信息
"=== System Info ===" | Out-File $logfile
Get-ComputerInfo | Select-Object WindowsVersion, OsHardwareAbstractionLayer | Out-File $logfile -Append

# 构建信息
"`n=== Build Info ===" | Out-File $logfile -Append
cargo --version | Out-File $logfile -Append
rustc --version | Out-File $logfile -Append

# 运行测试
"`n=== Test Output ===" | Out-File $logfile -Append
.\target\release\examples\test_kernel_boot.exe 2>&1 | Out-File $logfile -Append

Write-Host "Logs saved to $logfile"
```

---

## 🆘 获取帮助

如果以上方法都无法解决问题：

1. **查看文档**
   - [DEBUGGING_GUIDE.md](DEBUGGING_GUIDE.md)
   - [DEBUG_FINDINGS.md](DEBUG_FINDINGS.md)

2. **检查git历史**
```bash
git log --oneline | head -10
git show HEAD
```

3. **收集诊断信息**
   - 运行上面的诊断日志脚本
   - 保存所有日志文件
   - 记录错误信息

4. **提交Issue**
   - 包含诊断日志
   - 说明复现步骤
   - 描述预期行为

---

## ✅ 验证清单

运行测试前检查：

- [ ] Windows 11或更高版本
- [ ] Hyper-V已启用
- [ ] Rust工具链已安装
- [ ] libkrunfw.dll已构建
- [ ] test_kernel_boot.exe已构建
- [ ] 使用PowerShell或命令提示符（非Git Bash）
- [ ] RUST_LOG环境变量已设置

---

**最后更新:** 2026-03-18
**需要更多帮助？** 查看 [DOCS_INDEX.md](DOCS_INDEX.md)
