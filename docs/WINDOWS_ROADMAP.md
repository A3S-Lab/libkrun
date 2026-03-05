# libkrun Windows 支持研发计划

> 目标：将 libkrun 的 Windows 支持从实验阶段推进到生产可用

**当前完成度：~40%**
**预计总工作量：6-9 个月**

---

## 阶段 0：基础设施完善（2-3 周）

### 目标
稳定现有 WHPX 核心，建立测试框架

### 任务清单

#### 0.1 事件系统优化
- [x] **替换 polling 模拟** — 当前 1ms 轮询效率低
  - 方案 A：使用 Windows I/O Completion Ports (IOCP)
  - 方案 B：使用 `WaitForMultipleObjects` + 事件对象
  - 优先级：**高** | 工作量：3-5 天

- [x] **EventFd 改进** — 当前用 shared state + condvar
  - 改用 Windows Event Objects (`CreateEvent`)
  - 支持 edge-triggered 语义
  - 优先级：**中** | 工作量：2-3 天

#### 0.2 测试框架
- [ ] **单元测试** — 为 Windows 特定代码添加测试
  - 进度：`src/vmm/src/windows/`、`src/polly/src/event_manager_windows.rs`、`src/utils/src/windows/` 已补基础单元测试骨架
  - 进度：`whpx_vcpu` 已补 MMIO 解码与 ModRM/SIB 边界路径单元测试
  - `src/vmm/src/windows/` 的 WHPX 操作
  - `src/polly/src/event_manager_windows.rs` 的事件循环
  - `src/utils/src/windows/` 的工具函数
  - 优先级：**高** | 工作量：3-4 天

- [ ] **集成测试** — 端到端 VM 启动测试
  - 进度：已新增 WHPX 手动 smoke tests（`#[ignore]`，用于 Windows/Hyper-V 环境下验证 VM 创建和内存映射）
  - 进度：已新增 `tests/windows/run_whpx_smoke.ps1`，包含最小 rootfs 目录骨架和 WHPX smoke 执行入口
  - 进度：WHPX smoke 已输出日志与元数据（便于 CI artifact 回溯）
  - 进度：WHPX smoke 已输出阶段标记（prepare/run/assert）与最终状态文件（便于自动判定）
  - 进度：WHPX smoke 已支持复用预制 rootfs（基于 marker 文件，减少重复准备时间）
  - 进度：rootfs 复用已增加版本/时效策略（marker 格式不匹配或过期将自动重建）
  - 进度：marker 格式已参数化（支持按版本灰度切换 rootfs 复用策略）
  - 进度：已支持兼容 marker 列表（逗号分隔），可平滑过渡 rootfs 格式版本
  - 进度：已支持兼容 marker 自动升级到主版本（可开关），减少长期双版本维护
  - 进度：已支持 rootfs 决策 dry-run（仅判定复用/重建，不执行测试）
  - 进度：已支持 rootfs 重建策略门禁（可配置为遇到重建即失败）
  - 创建最小 rootfs
  - 测试 VM 启动 → 运行 → 关闭流程
  - 验证 MMIO/IO 端口处理
  - 优先级：**高** | 工作量：2-3 天

#### 0.3 CI/CD
- [ ] **GitHub Actions** — Windows 构建和测试
  - 进度：已新增 `.github/workflows/windows_ci.yml`（windows-latest，utils/polly/vmm 构建+Windows 模块单测）
  - 进度：已新增 `workflow_dispatch` 的 self-hosted Hyper-V smoke job（运行 `#[ignore]` WHPX 集成测试）
  - 进度：`workflow_dispatch` 已参数化（可开关 WHPX smoke，并传入测试过滤器）
  - 进度：`workflow_dispatch` 已支持传入 `rootfs_dir`（复用 runner 预制 rootfs）
  - 进度：`workflow_dispatch` 已支持 `cleanup_rootfs`（可选清理 smoke rootfs）
  - 进度：WHPX smoke job 已自动上传日志 artifact
  - 进度：WHPX smoke 已写入 GitHub Job Summary（状态 + 阶段时间线）
  - 进度：Job Summary 已优先解析 `summary.json`（并保留 `summary.txt` 兼容回退）
  - 进度：summary 已包含 runner 信息与 git SHA（便于跨 runner 回归定位）
  - 添加 `windows-latest` runner
  - 自动化测试运行
  - 优先级：**中** | 工作量：1-2 天

**阶段 0 交付物：**
- ✅ 稳定的事件系统
- ✅ 完整的测试覆盖
- ✅ 自动化 CI 流程

---

## 阶段 1：核心设备实现（4-6 周）

### 目标
实现基本可用的 Console、RNG、Balloon

### 任务清单

#### 1.1 Console 设备（virtio-console）
**当前状态：** Stub，无实际 I/O

- [ ] **Windows 终端集成**
  - 实现 `ReadConsoleW` / `WriteConsoleW` 集成
  - 处理 UTF-16 ↔ UTF-8 转换
  - 支持 ANSI 转义序列（通过 `SetConsoleMode` 启用 VT100）
  - 优先级：**高** | 工作量：5-7 天

- [ ] **Raw mode 支持**
  - 禁用行缓冲和回显（`ENABLE_LINE_INPUT`, `ENABLE_ECHO_INPUT`）
  - 处理 Ctrl+C / Ctrl+Break 信号
  - 优先级：**中** | 工作量：3-4 天

- [ ] **异步 I/O**
  - 使用 `ReadFile` / `WriteFile` 的 overlapped 模式
  - 集成到事件循环
  - 优先级：**高** | 工作量：4-5 天

**文件：** `src/devices/src/virtio/console_windows.rs` (当前 296 行)

#### 1.2 RNG 设备（virtio-rng）
**当前状态：** Stub，无熵源

- [ ] **Windows 熵源集成**
  - 使用 `BCryptGenRandom` (CNG API)
  - 实现 `FileReadVolatile` trait
  - 优先级：**中** | 工作量：2-3 天

- [ ] **性能优化**
  - 缓冲池避免频繁系统调用
  - 优先级：**低** | 工作量：1-2 天

**文件：** `src/devices/src/virtio/rng_windows.rs` (当前 62 行)

#### 1.3 Balloon 设备（virtio-balloon）
**当前状态：** Stub，无内存回收

- [ ] **Windows 内存管理**
  - 研究 `VirtualAlloc` / `VirtualFree` 的 `MEM_RESET` 标志
  - 或使用 `DiscardVirtualMemory` (Windows 8.1+)
  - 优先级：**低** | 工作量：3-5 天

- [ ] **WHPX 内存映射协调**
  - 确保与 `WHvMapGpaRange` 兼容
  - 处理内存回收后的重新映射
  - 优先级：**低** | 工作量：2-3 天

**文件：** `src/devices/src/virtio/balloon_windows.rs` (当前 62 行)

**阶段 1 交付物：**
- ✅ 可用的终端 I/O
- ✅ 功能完整的 RNG
- ✅ 基本的内存回收（可选）

---

## 阶段 2：网络支持（6-8 周）

### 目标
实现 virtio-net 和完整的 vsock

### 任务清单

#### 2.1 Vsock 增强（virtio-vsock）
**当前状态：** 仅 TCP stream 转发，无 Unix socket / TSI

- [ ] **Datagram 支持**
  - 实现 UDP 转发
  - 优先级：**中** | 工作量：3-4 天

- [ ] **Named Pipe 支持** — Windows 的 Unix socket 替代
  - 实现 `\\.\pipe\` 通信
  - 映射到 vsock 端口
  - 优先级：**高** | 工作量：5-7 天

- [ ] **TSI 支持** — 需要 libkrunfw Windows 内核
  - 移植 TSI 内核补丁到 Windows guest 内核
  - 实现 VMM 端的 socket 代理
  - 优先级：**低**（依赖 libkrunfw） | 工作量：10-15 天

**文件：** `src/devices/src/virtio/vsock_windows.rs` (当前 817 行)

#### 2.2 网络设备（virtio-net）
**当前状态：** 完全缺失

- [ ] **TAP 设备支持**
  - 使用 Windows TAP-Windows6 驱动（OpenVPN 项目）
  - 或使用 WinTUN（WireGuard 项目，性能更好）
  - 优先级：**高** | 工作量：10-14 天

- [ ] **设备模拟**
  - 实现 virtio-net 设备逻辑
  - TX/RX 队列处理
  - 优先级：**高** | 工作量：7-10 天

- [ ] **网络后端抽象**
  - 定义 Windows 特定的 `NetBackend` trait
  - 支持 TAP / WinTUN 切换
  - 优先级：**中** | 工作量：3-5 天

**新文件：** `src/devices/src/virtio/net_windows.rs` (预计 800-1200 行)

**阶段 2 交付物：**
- ✅ 完整的 vsock（含 datagram 和 named pipe）
- ✅ 可用的 virtio-net（基于 TAP 或 WinTUN）

---

## 阶段 3：文件系统支持（8-10 周）

### 目标
实现 virtio-fs 或替代方案

### 任务清单

#### 3.1 技术方案选择
- [ ] **方案评估**
  - 方案 A：移植 virtiofsd（需要 FUSE for Windows）
  - 方案 B：实现 9P 协议（Plan 9 filesystem protocol）
  - 方案 C：使用 SMB/CIFS 共享（性能较差）
  - 优先级：**高** | 工作量：3-5 天（调研）

#### 3.2 FUSE for Windows 集成（方案 A）
- [ ] **WinFsp 集成** — Windows 的 FUSE 实现
  - 安装和配置 WinFsp
  - 实现 FUSE 操作到 Windows 文件 API 的映射
  - 优先级：**高** | 工作量：10-15 天

- [ ] **virtiofsd 移植**
  - 修改 virtiofsd 以支持 Windows
  - 处理路径分隔符（`\` vs `/`）
  - 处理文件权限差异
  - 优先级：**高** | 工作量：15-20 天

#### 3.3 9P 协议实现（方案 B，备选）
- [ ] **9P 服务器**
  - 实现 9P2000.L 协议
  - 直接使用 Windows 文件 API
  - 优先级：**中** | 工作量：20-25 天

- [ ] **virtio-9p 设备**
  - 实现 virtio-9p 传输层
  - 集成到 libkrun
  - 优先级：**中** | 工作量：10-12 天

**新文件：** `src/devices/src/virtio/fs_windows.rs` 或 `9p_windows.rs`

**阶段 3 交付物：**
- ✅ 可用的文件系统共享（virtio-fs 或 9P）

---

## 阶段 4：高级功能（6-8 周）

### 目标
GPU、Sound、Input 等多媒体设备

### 任务清单

#### 4.1 GPU 设备（virtio-gpu）
- [ ] **Windows 显示后端**
  - 使用 GDI+ 或 Direct3D
  - 实现帧缓冲区到窗口的渲染
  - 优先级：**中** | 工作量：15-20 天

- [ ] **VirGL 支持**（可选）
  - 3D 加速支持
  - 优先级：**低** | 工作量：10-15 天

#### 4.2 Sound 设备（virtio-snd）
- [ ] **Windows 音频后端**
  - 使用 WASAPI (Windows Audio Session API)
  - 替代 Linux 的 PipeWire
  - 优先级：**低** | 工作量：10-12 天

#### 4.3 Input 设备（virtio-input）
- [ ] **Windows 输入处理**
  - 键盘/鼠标事件捕获
  - 使用 Raw Input API
  - 优先级：**低** | 工作量：5-7 天

**阶段 4 交付物：**
- ✅ 基本的 GPU 支持
- ✅ 音频输入/输出（可选）
- ✅ 输入设备支持（可选）

---

## 阶段 5：生产就绪（4-6 周）

### 目标
性能优化、文档、示例

### 任务清单

#### 5.1 性能优化
- [ ] **内存映射优化**
  - 使用 Large Pages (2MB)
  - 优先级：**中** | 工作量：3-5 天

- [ ] **中断注入优化**
  - 批量中断处理
  - 优先级：**低** | 工作量：2-3 天

- [ ] **设备 I/O 优化**
  - 减少 VM exit 次数
  - 优先级：**中** | 工作量：5-7 天

#### 5.2 文档和示例
- [ ] **API 文档**
  - Windows 特定的 API 说明
  - 优先级：**高** | 工作量：3-5 天

- [ ] **示例程序**
  - 最小 VM 启动示例
  - 网络/文件系统集成示例
  - 优先级：**高** | 工作量：5-7 天

- [ ] **故障排查指南**
  - 常见问题和解决方案
  - 优先级：**中** | 工作量：2-3 天

#### 5.3 发布准备
- [ ] **版本标记**
  - 从 "experimental" 升级到 "stable"
  - 优先级：**高** | 工作量：1 天

- [ ] **发布说明**
  - 功能列表、限制、已知问题
  - 优先级：**高** | 工作量：2-3 天

**阶段 5 交付物：**
- ✅ 生产级性能
- ✅ 完整文档
- ✅ 正式发布

---

## 依赖关系图

```
阶段 0 (基础设施)
    ↓
阶段 1 (核心设备) ← 必须完成
    ↓
    ├─→ 阶段 2 (网络) ← 高优先级
    ├─→ 阶段 3 (文件系统) ← 高优先级
    └─→ 阶段 4 (多媒体) ← 低优先级
         ↓
    阶段 5 (生产就绪)
```

---

## 资源需求

### 人力
- **核心开发者**：2-3 人（全职）
- **测试工程师**：1 人（兼职）
- **文档工程师**：1 人（兼职）

### 硬件
- Windows 10/11 Pro（支持 Hyper-V）
- 至少 16GB RAM
- 支持 VT-x/AMD-V 的 CPU

### 软件依赖
- Rust toolchain (MSVC target)
- Windows SDK
- Visual Studio Build Tools
- WinTUN / TAP-Windows6
- WinFsp（文件系统阶段）

---

## 风险评估

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| WHPX API 限制 | 高 | 早期原型验证，必要时调整架构 |
| 文件系统方案不可行 | 高 | 准备多个备选方案（FUSE/9P/SMB） |
| 性能不达标 | 中 | 持续性能测试，优化热点路径 |
| Windows 版本兼容性 | 中 | 明确最低支持版本（Windows 10 2004+） |
| 第三方依赖不稳定 | 低 | 选择成熟的开源项目（WinTUN, WinFsp） |

---

## 里程碑

| 里程碑 | 预计完成时间 | 标志 |
|--------|--------------|------|
| M1: 基础稳定 | 第 3 周 | 测试通过率 > 90% |
| M2: 核心设备可用 | 第 9 周 | Console + RNG 功能完整 |
| M3: 网络可用 | 第 17 周 | virtio-net 基本功能 |
| M4: 文件系统可用 | 第 27 周 | virtio-fs 或 9P 可用 |
| M5: 生产就绪 | 第 33 周 | 正式发布 v1.0-windows |

---

## 成功标准

### 功能完整性
- ✅ 所有核心 VirtIO 设备可用（console, net, fs, vsock, rng）
- ✅ 与 Linux/macOS 版本功能对等（除平台特定功能）

### 性能指标
- ✅ VM 启动时间 < 500ms
- ✅ 网络吞吐量 > 1Gbps
- ✅ 文件系统 I/O 性能 > 500MB/s

### 稳定性
- ✅ 连续运行 24 小时无崩溃
- ✅ 测试覆盖率 > 80%

### 文档
- ✅ 完整的 API 文档
- ✅ 至少 3 个工作示例
- ✅ 故障排查指南

---

## 下一步行动

1. **立即开始：** 阶段 0.1 事件系统优化
2. **并行进行：** 阶段 0.2 测试框架搭建
3. **技术调研：** 阶段 3.1 文件系统方案评估（可提前进行）

---

*本计划基于当前代码分析，实际执行中可能需要调整。*
