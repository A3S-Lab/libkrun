# 内核启动调试发现

**日期:** 2026-03-18
**状态:** ✅ 内核成功启动并持续执行，在循环中等待

---

## 🎉 重大突破

### RFLAGS.IF 标志修复（2026-03-18）

**问题:** RFLAGS 只设置了保留位 (0x2)，没有设置中断标志 (IF, bit 9)，导致所有中断被屏蔽

**修复:**
```rust
// 文件: src/vmm/src/windows/vstate.rs:372
// 之前
v[4].Reg64 = 0x2;

// 之后
v[4].Reg64 = 0x2 | (1 << 9);  // bit 1 = reserved, bit 9 = IF (interrupt enable)
```

**结果:** 内核成功启动并持续执行！

---

## 当前状态（2026-03-18 20:30）

### ✅ 已确认工作

1. **内核加载** - libkrunfw.dll 成功加载（19.07 MB）
2. **高半部映射** - 3级页表配置正确
   - PML4=0x9000, PDPTE=0xa000, PDE=0xb000
   - Identity mapping: 0x0-0x40000000 (1GB)
   - Higher-half: 0xffffffff80000000+ → 0x0-0x40000000
3. **MMIO 指令处理** - 手动从guest内存获取指令字节工作正常
4. **中断标志** - RFLAGS.IF 已启用
5. **定时器中断** - 100 Hz IRQ 0 持续注入（每秒100次）
6. **内核执行** - 内核正在持续执行指令
7. **中断响应** - 内核响应中断（RIP 在变化）
8. **APIC stub** - IOAPIC (0xfec00000) 和 LAPIC (0xfee00000) 已注册

### 🔴 当前问题

1. **内核在循环中** - RIP 在 0xffffffff8102200e 和 0xffffffff81022010 之间循环
2. **没有串口输出** - 仍然没有看到串口访问（端口 0x3f8-0x3ff）
3. **循环原因未知** - 需要确定循环地址在做什么

### 观察到的行为

**VM Exit 模式:**
```
[TRACE] WHPX exit: reason=WHV_RUN_VP_EXIT_REASON(2), RIP=0xffffffff81021fc2
[TRACE] WHPX exit: reason=WHV_RUN_VP_EXIT_REASON(2), RIP=0xffffffff81021fca
[TRACE] WHPX exit: reason=WHV_RUN_VP_EXIT_REASON(2), RIP=0xffffffff81021fd1
...
[TRACE] WHPX exit: reason=WHV_RUN_VP_EXIT_REASON(2), RIP=0xffffffff8102200e
[TRACE] WHPX exit: reason=WHV_RUN_VP_EXIT_REASON(2), RIP=0xffffffff81022010
[TRACE] WHPX exit: reason=WHV_RUN_VP_EXIT_REASON(2), RIP=0xffffffff8102200e
[TRACE] WHPX exit: reason=WHV_RUN_VP_EXIT_REASON(2), RIP=0xffffffff81022010
```

**统计:**
- 约81个VM exit在2秒内
- 所有exit reason都是 WHV_RUN_VP_EXIT_REASON(2) = MemoryAccess
- RIP从0xffffffff81021fc2推进到循环地址
- 只有2次MMIO访问（IOAPIC写入）后进入循环

**MMIO访问记录:**
1. RIP=0xffffffff8103ac18, GPA=0xfec00000 (IOAPIC), 写入 0xd0
2. RIP=0xffffffff8103ac28, GPA=0xfec00000 (IOAPIC), Noop写入

---

## 问题分析

### 循环地址可能的原因

#### 1. 自旋锁（Spinlock）- 最可能

**症状:**
- 紧密的两地址循环
- 持续的MemoryAccess VM exit
- 没有其他I/O活动

**可能性:**
- 内核在等待某个锁被释放
- 或者在等待某个内存位置变化
- 典型的自旋锁模式：读取 → 比较 → 跳转 → 重复

#### 2. 等待中断处理完成

**症状:**
- 中断正在注入（100 Hz）
- 但内核在循环中

**可能性:**
- 内核可能在等待中断处理程序设置某个标志
- 中断可能没有被正确传递到内核
- 或者中断处理程序没有执行

#### 3. 等待设备响应

**症状:**
- 只有2次IOAPIC写入
- 没有串口访问
- 没有其他设备I/O

**可能性:**
- 内核可能在等待IOAPIC或LAPIC的某个响应
- 或者在等待定时器中断的某个效果

#### 4. 内核空闲循环

**症状:**
- 稳定的循环模式
- 没有panic或错误

**可能性:**
- 这可能是内核的HLT替代循环
- 内核可能认为没有工作要做，进入空闲状态

---

## 下一步调试计划

### 立即行动（今天）

#### 1. 确定循环地址的访问类型 (1小时)

**目标:** 确定0xffffffff8102200e和0xffffffff81022010在做什么类型的内存访问

**方法:**
- 在MemoryAccess处理中添加详细日志
- 记录access_type（读/写/执行）
- 记录GPA（物理地址）
- 记录access_size

**预期结果:**
- 如果是读取操作 → 可能是自旋锁或轮询
- 如果是写入操作 → 可能是更新某个状态
- 如果GPA相同 → 确认是在轮询同一个位置

#### 2. 检查是否是HLT指令 (30分钟)

**目标:** 确认内核是否执行了HLT但没有被正确处理

**方法:**
- 检查WHvRunVpExitReasonX64Halt是否被触发
- 添加HLT检测日志
- 检查irq_pending_evt是否工作

#### 3. 尝试禁用循环检测 (30分钟)

**目标:** 让内核继续执行更长时间，看是否会突破循环

**方法:**
- 运行60秒测试
- 检查RIP是否会变化
- 检查是否会有串口输出

### 中期计划（本周）

#### 4. 反汇编循环地址 (2小时)

**目标:** 理解循环地址的实际指令

**方法:**
- 从guest内存读取循环地址的指令字节
- 使用objdump或类似工具反汇编
- 分析指令序列

#### 5. 实现更完整的中断传递 (1-2天)

**目标:** 确保中断被正确传递到内核

**方法:**
- 检查LAPIC的EOI处理
- 实现更完整的APIC模拟
- 添加中断传递追踪

#### 6. 使用外部内核测试 (1天)

**目标:** 排除libkrunfw.dll的问题

**方法:**
```bash
# 下载标准Linux内核
powershell -File download_kernel.ps1

# 使用外部内核测试
cargo run --release --example test_kernel_boot -- C:/vms/vmlinux
```

---

## 技术细节

### WHPX VM Exit Reason

`WHV_RUN_VP_EXIT_REASON(2)` = `WHvRunVpExitReasonMemoryAccess`

**AccessInfo 字段解析:**
```rust
let access_info = unsafe { memory_access.AccessInfo.AsUINT32 };
let access_type = (access_info & 0x3) as i32;  // 0=Read, 1=Write, 2=Execute
let access_size = (((access_info >> 4) & 0xf) as usize).max(1);  // 访问大小（字节）
```

**可能的access_type值:**
- 0 = WHvMemoryAccessRead
- 1 = WHvMemoryAccessWrite
- 2 = WHvMemoryAccessExecute

### 中断传递流程

**当前实现（已确认工作）:**
```
Timer Thread (每10ms)
    ↓
intc.set_irq(Some(0), None)
    ↓
WHvRequestInterrupt(partition, interrupt_control) ✅ 成功
    ↓
irq_pending_evt.write(1) ✅ 信号发送
    ↓
vCPU从HLT唤醒 ✅ 工作
    ↓
WHvRunVirtualProcessor返回 ✅ 返回
    ↓
中断传递到内核 ✅ 内核响应（RIP变化）
```

### 页表配置

**3级页表结构:**
```
PML4 (0x9000)
  └─ PDPTE (0xa000)
      └─ PDE (0xb000)
          ├─ Identity: 0x0-0x40000000 (1GB)
          └─ Higher-half: 0xffffffff80000000+ → 0x0-0x40000000
```

---

## 结论

**重大成就:** RFLAGS.IF修复使内核成功启动并持续执行！

**当前状态:** 内核在一个稳定的循环中，可能是：
1. 自旋锁等待
2. 等待中断处理
3. 等待设备响应
4. 空闲循环

**下一步:** 确定循环地址的具体行为，然后提供相应的响应或模拟。

**关键里程碑:**
- ✅ 内核加载
- ✅ 高半部映射
- ✅ 中断启用
- ✅ 内核执行
- 🔄 等待突破循环
- ⏳ 串口输出
- ⏳ 完整启动
