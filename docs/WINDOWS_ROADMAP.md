# libkrun Windows 支持研发计划

> 目标：将 libkrun 的 Windows 支持从实验阶段推进到生产可用

**当前完成度：~95%**
**最后更新：2026-03-10**

---

## 当前状态总览

Windows 后端（基于 WHPX）已达到生产可用状态，核心虚拟化能力和所有关键 virtio 设备均已实现。

| 功能模块 | 状态 | 说明 |
|----------|------|------|
| WHPX 核心虚拟化 | ✅ 完成 | 分区管理、内存映射、vCPU、VM Exit 处理、MSR/CPUID 模拟 |
| 事件系统 | ✅ 完成 | IOCP / Windows Event Objects，替代原始 polling |
| virtio-console | ✅ 完成 | 多端口、stdin/stdout/file 输出、异步 I/O |
| virtio-block | ✅ 完成 | 读写、flush、sparse file |
| virtio-fs | ✅ 完成 | 完整 FUSE 实现，读写/目录/symlink/fsync/fallocate |
| virtio-net | ✅ 完成 | TcpStream 后端，checksum offload，TSO 协商 |
| virtio-vsock | ✅ 完成 | TSI（TCP/UDP）、Named Pipe 后端（AF_UNIX）、DGRAM |
| virtio-balloon | ✅ 完成 | inflate/deflate、free-page reporting、page-hinting |
| virtio-rng | ✅ 完成 | BCryptGenRandom |
| virtio-snd | ⚠️ 部分 | NullBackend（无实际音频输出） |
| virtio-gpu | ❌ 未实现 | 需要 rutabaga_gfx Windows 支持 |
| virtio-input | ❌ 未实现 | 键盘/鼠标输入捕获 |
| CI/CD | ✅ 完成 | GitHub Actions：构建 + 单测 + WHPX smoke（self-hosted） |
| 测试框架 | ✅ 完成 | 40+ WHPX smoke tests、集成测试脚本、CI artifact 上传 |

---

## 已完成工作

### 阶段 0：基础设施 ✅

- [x] 事件系统：替换 1ms polling，改用 IOCP / `WaitForMultipleObjects`
- [x] EventFd：改用 Windows Event Objects，支持 edge-triggered 语义
- [x] 单元测试骨架：`src/vmm/src/windows/`、`src/polly/`、`src/utils/src/windows/`
- [x] WHPX smoke tests：VM 创建、内存映射、MMIO 解码、ModRM/SIB 边界路径
- [x] 集成测试脚本：`tests/windows/run_whpx_smoke.ps1`，支持 rootfs 复用/版本策略/dry-run
- [x] GitHub Actions：`windows_ci.yml`，构建、单测、self-hosted WHPX smoke、artifact 上传、Job Summary

### 阶段 1：核心设备 ✅

- [x] virtio-console：Windows 终端集成、raw mode、overlapped 异步 I/O
- [x] virtio-rng：BCryptGenRandom 熵源
- [x] virtio-balloon：inflate/deflate、free-page reporting、page-hinting

### 阶段 2���网络支持 ✅

- [x] virtio-vsock：TSI（TCP/UDP）、Named Pipe 后端（AF_UNIX）、DGRAM、credit flow control
- [x] virtio-net：TcpStream 后端、TX/RX 队列、checksum offload、TSO 协商

### 阶段 3：文件系统支持 ✅

- [x] virtio-fs Phase 1：核心数据结构、只读目录操作（lookup、readdir、getattr）
- [x] virtio-fs Phase 2：文件读取（open、read、release、statfs）
- [x] virtio-fs Phase 3：写操作（create、write、unlink、mkdir、rmdir、rename、setattr）
- [x] virtio-fs Phase 4：高级功能（flush、fsync、symlink、readlink、access、lseek、fallocate）
- [x] Windows 适配：GetDiskFreeSpaceExW、符号链接权限、文件属性映射、sync_all/sync_data

---

## 剩余工作

### 近期

#### 1. multi-vCPU（SMP）支持
- [ ] 实现 INIT/SIPI AP 启动协议
- [ ] 跨 vCPU 中断投递
- 优先级：**高** | 工作量：7-10 天

#### 2. libkrunfw-windows
- [ ] 发布预编译 x86_64 ELF vmlinux 的 Windows companion library
- [ ] 消除调用方自行提供 kernel 的需求（对齐 Linux/macOS 体验）
- 优先级：**高** | 工作量：5-7 天

#### 3. virtio-fs 缺失 syscall
**文件：** `src/devices/src/virtio/fs/windows/passthrough.rs`

- [ ] `link`：硬链接，可用 `CreateHardLinkW` 实现，工作量 1-2 天
- [ ] `mknod`：特殊文件创建，Windows 无原生等价，可返回 `ENOSYS`，工作量 2-3 天
- [ ] `copy_file_range`：可用 `CopyFileExW` 或分块读写模拟，工作量 2-3 天
- 优先级：**中**

#### 4. virtio-net TSO 实际分包
- [ ] TcpStream 侧实现 TCP segment 拆分（当前仅完成协商）
- 优先级：**中** | 工作量：3-5 天

### 中期

#### 5. virtio-snd WASAPI 后端
- [ ] 集成 Windows Audio Session API 替代 NullBackend
- 优先级：**低** | 工作量：10-12 天

#### 6. virtio-gpu Windows 后端
- [ ] 跟踪上游 rutabaga_gfx Windows 支持进展
- [ ] 备选：WGPU 后端
- 优先级：**低**（外部依赖）

#### 7. 文档和示例
- [ ] Windows 特定 API 文档（`krun_add_net_tcp`、`krun_add_vsock_port_windows` 等）
- [ ] 最小 VM 启动示例（PowerShell / C）
- [ ] 故障排查指南
- 优先级：**中** | 工作量：3-5 天

### 长期

#### 8. Windows ARM64 支持
- [ ] 跟踪 Microsoft WHPX ARM64 partition type 支持进度
- 优先级：**低**（外部依赖）

---

## 已知限制

| 功能 | 原因 |
|------|------|
| 单 vCPU | SMP 支持待实现 |
| 无 libkrunfw-windows | 调用方需自行提供 kernel |
| virtio-gpu 不支持 | 依赖上游 rutabaga_gfx |
| virtio-snd 无音频输出 | NullBackend，WASAPI 集成待做 |
| virtio-input 不支持 | 优先级低，暂缓 |
| x86_64 only | Windows on ARM WHPX 不支持 ARM64 partition |

---

*基于 commit: 54f2719（2026-03-10）*
