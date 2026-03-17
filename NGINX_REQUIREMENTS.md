# 运行 Nginx 所需功能分析

**当前日期:** 2026-03-18
**当前状态:** 内核可以启动并接收定时器中断，但无法看到串口输出

---

## 当前已完成功能 ✅

1. **内核加载** ✅
   - libkrunfw.dll 加载成功
   - 内核加载到 guest 内存 (0x1000000)
   - 入口点设置正确 (0x1000123)

2. **高半部内核映射** ✅
   - 3 级页表结构 (PML4, PDPTE, PDE)
   - 恒等映射: 0x0 → 0x40000000 (1GB)
   - 高半部映射: 0xffffffff80000000+ → 0x0-0x40000000

3. **MMIO 指令处理** ✅
   - 手动从 guest 内存获取指令字节
   - 高半部地址直接转换
   - 正确的 RIP 推进

4. **PIT 定时器中断** ✅
   - 100 Hz IRQ 0 注入
   - WHvRequestInterrupt API 工作正常
   - vCPU 可以从 HLT 状态唤醒

5. **设备模拟** ✅
   - PIT 8254 (端口 0x40-0x43)
   - PIC 8259A (端口 0x20-0x21, 0xA0-0xA1)
   - IOAPIC stub (MMIO 0xfec00000)
   - LAPIC stub (MMIO 0xfee00000)
   - Serial COM1 (端口 0x3F8-0x3FF)

---

## 当前问题 ⚠️

### 1. 无串口输出
**现象:**
- 内核启动后没有任何串口输出
- 无法看到内核启动日志
- 无法确认内核是否真正进入用户空间

**可能原因:**
1. 内核可能在早期启动阶段挂起
2. 串口中断 (IRQ 4) 未实现
3. 串口设备可能需要更完整的模拟
4. 内核可能在等待某些硬件初始化

**调试方法:**
```bash
# 使用 debug 日志查看详细的 MMIO/IO 访问
RUST_LOG=debug cargo run --release --example test_kernel_boot 2>&1 | tee kernel_debug.log

# 查看是否有串口端口访问
grep "0x3f8\|COM1\|serial" kernel_debug.log
```

### 2. 内核可能未完成启动
**现象:**
- 测试运行 30 秒后超时
- 没有看到 "Kernel booted successfully!" 消息
- init 进程可能未执行

**可能原因:**
1. 内核在等待某些设备初始化
2. 缺少必要的中断支持
3. 内存或 I/O 访问问题
4. 内核 panic 但没有输出

---

## 运行 Nginx 所需的额外功能

### 必需功能 (Critical)

#### 1. 串口输出工作 🔴
**优先级:** 最高
**原因:** 没有串口输出就无法调试和确认内核状态

**需要实现:**
- 确认串口设备正确注册
- 可能需要实现串口中断 (IRQ 4)
- 验证串口数据路径 (guest → host stdout)

**测试方法:**
```rust
// 在 test_kernel_boot.rs 中添加
println!("[DEBUG] Serial devices configured: {}", serial_devices.len());
```

#### 2. 内核完整启动到用户空间 🔴
**优先级:** 最高
**原因:** 必须能够执行 init 进程才能运行任何用户程序

**需要实现:**
- 确认内核完成所有初始化阶段
- 确认 init 进程被执行
- 确认可以看到 init 脚本的输出

**验证标志:**
```
[    0.000000] Linux version 5.x.x ...
[    0.000000] Command line: console=ttyS0 ...
...
[    X.XXXXXX] Run /init as init process
Kernel booted successfully!  <-- 来自 init 脚本
```

#### 3. 文件系统支持 🔴
**优先级:** 高
**原因:** Nginx 需要读取配置文件、HTML 文件等

**当前状态:**
- 测试使用临时目录作为 rootfs
- 只有一个简单的 init 脚本
- 没有完整的 Linux 文件系统结构

**需要实现:**
- 完整的 rootfs (包含 /bin, /lib, /etc, /usr 等)
- Nginx 二进制文件和依赖库
- Nginx 配置文件
- 可能需要 virtio-fs 或 9p 文件系统

**创建 rootfs:**
```bash
# 方案 1: 使用 Alpine Linux minirootfs
wget https://dl-cdn.alpinelinux.org/alpine/v3.19/releases/x86_64/alpine-minirootfs-3.19.0-x86_64.tar.gz
mkdir rootfs
tar -xzf alpine-minirootfs-3.19.0-x86_64.tar.gz -C rootfs/
apk add --root rootfs nginx

# 方案 2: 使用 Docker 导出
docker create --name nginx-rootfs nginx:alpine
docker export nginx-rootfs | tar -xC rootfs/
docker rm nginx-rootfs
```

#### 4. 网络设备 🔴
**优先级:** 高
**原因:** Nginx 需要监听网络端口

**需要实现:**
- virtio-net 设备
- 或者 TAP/TUN 网络接口
- 网络中断支持

**当前状态:**
- 代码中可能已有 virtio-net 支持
- 需要验证在 Windows WHPX 上是否工作

#### 5. 更多中断支持 🟡
**优先级:** 中
**原因:** 设备需要中断来通知内核

**需要实现:**
- 串口中断 (IRQ 4 for COM1)
- 网络设备中断
- virtio 设备中断

### 可选功能 (Nice to Have)

#### 6. 多 vCPU 支持 🟢
**优先级:** 低
**原因:** Nginx 可以利用多核提高性能

**需要实现:**
- 创建多个 vCPU
- IPI (Inter-Processor Interrupt) 支持
- 每个 vCPU 的 LAPIC 模拟

#### 7. 更完整的设备模拟 🟢
**优先级:** 低
**原因:** 提高兼容性

**可能需要:**
- RTC (Real-Time Clock)
- ACPI 支持
- PCI 设备枚举

---

## 调试步骤

### 第一步: 确认串口输出
```bash
# 1. 运行测试并捕获所有输出
RUST_LOG=debug cargo run --release --example test_kernel_boot 2>&1 | tee full_output.log

# 2. 查找串口相关日志
grep -i "serial\|COM1\|0x3f8" full_output.log

# 3. 查找 I/O 端口访问
grep "I/O.*0x3f" full_output.log

# 4. 查找内核输出
grep "Linux\|Kernel\|init" full_output.log
```

### 第二步: 添加更多调试日志
在 `src/devices/src/legacy/x86_64/serial.rs` 中添加:
```rust
pub fn write(&mut self, offset: u64, data: &[u8]) {
    eprintln!("[SERIAL DEBUG] Write to offset {:#x}, data: {:?}", offset, data);
    // ... 现有代码
}

pub fn read(&mut self, offset: u64, data: &mut [u8]) {
    eprintln!("[SERIAL DEBUG] Read from offset {:#x}", offset);
    // ... 现有代码
}
```

### 第三步: 检查内核是否真的在运行
在 `src/vmm/src/windows/whpx_vcpu.rs` 中添加周期性日志:
```rust
static mut INSTRUCTION_COUNT: u64 = 0;
unsafe {
    INSTRUCTION_COUNT += 1;
    if INSTRUCTION_COUNT % 10000 == 0 {
        eprintln!("[VCPU] Executed {} instructions, current RIP: {:#x}",
                  INSTRUCTION_COUNT, exit_context.VpContext.Rip);
    }
}
```

### 第四步: 使用外部内核测试
```bash
# 下载预编译的 Linux 内核
# 从 https://github.com/containers/libkrunfw/releases

# 使用外部内核运行
cargo run --release --example test_kernel_boot -- C:/vms/vmlinux
```

---

## 最小可行路径 (MVP)

要运行 Nginx，按优先级排序的实现步骤:

### Phase 1: 确认内核启动 (1-2 天)
1. ✅ 内核加载和映射 (已完成)
2. ✅ 定时器中断 (已完成)
3. 🔴 **串口输出工作** (当前阻塞)
4. 🔴 **内核完整启动到用户空间** (当前阻塞)

### Phase 2: 基本用户空间 (2-3 天)
5. 🔴 创建完整的 rootfs
6. 🔴 确认可以执行简单的 shell 命令
7. 🔴 确认文件系统读写正常

### Phase 3: 网络支持 (3-5 天)
8. 🔴 实现 virtio-net 设备
9. 🔴 配置网络接口
10. 🔴 测试基本网络连接

### Phase 4: Nginx 运行 (1-2 天)
11. 🔴 安装 Nginx 到 rootfs
12. 🔴 配置 Nginx
13. 🔴 启动 Nginx 并测试

**总计估算:** 7-12 天

---

## 立即行动项

### 今天应该做的:

1. **调试串口输出** (2-4 小时)
   - 添加串口调试日志
   - 确认串口设备是否被访问
   - 检查数据是否到达 stdout

2. **确认内核状态** (1-2 小时)
   - 添加指令计数器
   - 查看 RIP 是否在推进
   - 确认内核没有 panic 或挂起

3. **如果串口工作** (2-3 小时)
   - 等待内核完整启动
   - 查看内核日志
   - 确认 init 进程执行

4. **如果串口不工作** (4-6 小时)
   - 实现串口中断 (IRQ 4)
   - 修复串口设备模拟
   - 或者使用其他调试方法 (MMIO 日志分析)

---

## 结论

**当前最大阻塞:** 无法看到串口输出，无法确认内核状态

**下一步:** 调试串口输出，确认内核是否真正启动到用户空间

**运行 Nginx 的关键路径:**
```
串口输出 → 内核完整启动 → 用户空间 → 文件系统 → 网络 → Nginx
   🔴          🔴           🔴        🔴       🔴      🔴
 (当前)      (未知)       (未知)    (缺失)   (缺失)  (目标)
```

一旦串口输出工作，我们就能看到内核的详细启动日志，从而确定还需要实现哪些功能。
