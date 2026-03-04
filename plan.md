# Windows WHPX Backend — Implementation Plan

Branch: `chore/windows-ci-smoke-validation`

## 状态总览

| 层次 | 状态 |
|------|------|
| WHPX VM/vCPU 基础设施 | ✅ 完成 |
| ELF 内核加载 + boot params | ✅ 完成 |
| IO 端口：未注册端口静默处理 | ✅ 完成 |
| IO 端口：串口 COM1 输出捕获 | ✅ 完成 |
| virtio-blk Windows 后端 | ✅ 完成 |
| virtio-net Windows 后端 | ✅ 完成 |
| virtio-console 输入/输出 | ✅ 完成 |
| virtio-vsock Windows 后端 | ✅ 完成 |
| `krun_add_disk` / `krun_add_net` Windows API | ✅ 完成 |
| e2e 真实内核启动测试框架 | ✅ 完成（Linux version banner 已验证） |
| MMIO 未注册地址 → Stopped | ✅ 完成 |
| 删除死代码 `run_emulation()` | ✅ 完成 |
| 下载脚本 URL 更新 | ✅ 完成 |
| **中断投递（PIC + PIT + LAPIC）** | 🔧 **下一目标** |
| PIT timer 注册（0x40-0x43） | ⬜ 待实现 |
| 完整启动到 userspace | ⬜ 阻塞于中断 |
| e2e 测试加入 CI | ⬜ 待加入 |
| 删除死代码 `run_emulation()` | ⬜ 待清理 |
| 下载脚本 URL 更新 | ⬜ 待更新 |

---

## 当前任务：MMIO 未注册 → Stopped 修复

**文件**：`src/vmm/src/windows/vstate.rs`，`run()` 方法

### 问题

`VcpuExit::MmioRead` / `VcpuExit::MmioWrite` 在以下两种情况下返回 `VcpuEmulation::Stopped`，直接终止 vCPU 线程：

1. `mmio_bus` 为 `None`（测试场景或设备未注册时）
2. `mmio_bus.read/write()` 返回 `false`（地址未被任何设备注册）

```
MmioRead(addr, data):
  if bus is None        → Stopped   ← BUG
  if bus.read() = false → Stopped   ← BUG

MmioWrite(addr, data):
  if bus is None         → Stopped  ← BUG
  if bus.write() = false → Stopped  ← BUG
```

IO 端口已在之前修复为"始终 Handled"，MMIO 未同步。

### 修复方案

对齐 IO 端口的已有实现：

- **MmioRead**：无论 bus 是否注册，始终调用 `complete_mmio_read`（未注册时用零值完成），返回 `Handled`
- **MmioWrite**：无论 bus 是否注册，始终调用 `complete_mmio_write`，返回 `Handled`
- 保留借用规则：先 copy data 到本地缓冲区，`let _ = data` 释放借用，再调用 `complete_mmio_read`

### 修复后结构

```rust
VcpuExit::MmioRead(addr, data) => {
    if let Some(mmio_bus) = &self.mmio_bus {
        mmio_bus.read(self.id as u64, addr, data); // 未注册时 data 保持为零
    }
    let mut completion = [0_u8; 8];
    completion[..data.len()].copy_from_slice(data);
    let len = data.len();
    let _ = data;
    if let Err(e) = self.whpx_vcpu.complete_mmio_read(&completion[..len]) {
        // 仅 complete 失败时才 Stopped
        self.whpx_vcpu.clear_pending_mmio();
        VcpuEmulation::Stopped
    } else {
        VcpuEmulation::Handled
    }
}

VcpuExit::MmioWrite(addr, data) => {
    if let Some(mmio_bus) = &self.mmio_bus {
        mmio_bus.write(self.id as u64, addr, data);
    }
    let _ = data;
    if let Err(e) = self.whpx_vcpu.complete_mmio_write() {
        self.whpx_vcpu.clear_pending_mmio();
        VcpuEmulation::Stopped
    } else {
        VcpuEmulation::Handled
    }
}
```

---

## 后续任务（按优先级）

### P1：删除死代码 `run_emulation()`

`src/vmm/src/windows/vstate.rs` 第 412-489 行的 `pub fn run_emulation()` 无任何调用方，
且含有旧的 Stopped-for-unregistered-IO bug，直接删除。

### P2：PIC 8259A 注册（0x20-0x21, 0xA0-0xA1）

`src/vmm/src/builder.rs` `attach_legacy_devices`（Windows 路径）需要注册 PIC，
内核 early boot 会探测这些端口。

### P3：PIT 8253 timer 注册（0x40-0x43）

Linux 使用 PIT 校准 TSC 并驱动 scheduler tick。
没有 PIT，内核卡在 `tsc: Fast TSC calibration failed`。

### P4：中断注入（`WHvRequestInterrupt`）

PIT IRQ0 产生后需要通过 WHPX API 注入 vCPU：
- `WHvRequestInterrupt(partition, &interrupt_control, size)`
- 需要维护 PIC/IOAPIC 中断路由表

### P5：e2e 测试加入 CI + 下载脚本 URL 修复

- `tests/windows/download_test_kernel.ps1` URL 更新为：
  `https://s3.amazonaws.com/spec.ccfc.min/img/hello/kernel/hello-vmlinux.bin`
- `.github/workflows/windows_ci.yml` 加入 `test_whpx_real_kernel_e2e` 步骤
