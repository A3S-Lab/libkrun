# TSI Windows 实现可行性分析

## 执行摘要

**结论：在 Windows 上实现完整的 TSI 功能在技术上可行，但需要大量工作（估计 4-8 周）。建议优先评估是否真正需要 TSI，或者 virtio-net 是否足够。**

## TSI 技术背景

### 什么是 TSI？

TSI (Transparent Socket Impersonation) 是 libkrun 的核心创新，允许 guest 进程直接使用宿主机的网络栈，无需虚拟网卡。

**工作原理：**
1. Guest 内核通过 vsock 发送特殊的 TSI 命令（TSI_CONNECT, TSI_LISTEN 等）
2. Host 端的 vsock 设备拦截这些命令
3. Host 代表 guest 创建真实的 socket（TCP/UDP/Unix）
4. 数据通过 vsock 在 guest 和 host socket 之间透明传输

### 当前实现（Linux/macOS）

**核心组件：**
- `tsi_stream.rs`: TCP/Unix socket 代理
- `tsi_dgram.rs`: UDP socket 代理
- `muxer.rs`: TSI 命令处理和路由
- `proxy.rs`: 代理抽象层

**依赖：**
- `nix` crate: Unix 系统调用封装
- `std::os::unix`: Unix 特定 API
- Raw file descriptors (RawFd)
- Unix domain sockets
- POSIX socket API

## Windows 实现挑战

### 1. API 差异

| 功能 | Linux/macOS | Windows | 差距 |
|------|-------------|---------|------|
| Socket 创建 | `socket()` | `WSASocket()` | 不同 API |
| 非阻塞 I/O | `fcntl(O_NONBLOCK)` | `ioctlsocket(FIONBIO)` | 不同机制 |
| 文件描述符 | `RawFd` (int) | `SOCKET` (HANDLE) | 类型不兼容 |
| Unix sockets | `AF_UNIX` | Named Pipes | 完全不同 |
| 事件通知 | `epoll` | `IOCP` / `select` | 不同模型 |

### 2. 架构差异

**Linux/macOS 架构：**
```
Guest Kernel → vsock → TsiStreamProxy → Unix Socket API → Host Network
```

**Windows 需要的架构：**
```
Guest Kernel → vsock → TsiStreamProxy (Windows) → Winsock2 API → Host Network
```

### 3. 代码重写范围

需要重写的模块：
- ✅ `tsi_stream.rs`: 完全重写（~500 行）
- ✅ `tsi_dgram.rs`: 完全重写（~300 行）
- ⚠️ `muxer.rs`: 部分修改（TSI 命令处理）
- ⚠️ `proxy.rs`: 接口适配
- ✅ 新增 `tsi_windows.rs`: Windows 特定实现

**估计工作量：**
- 核心实现：2-3 周
- 测试和调试：1-2 周
- 文档和集成：1 周
- **总计：4-6 周**

## 实现方案

### 方案 A：完整 TSI 实现（推荐）

**优点：**
- 功能完整，与 Linux/macOS 对等
- 最佳性能和透明性
- 支持所有 socket 类型（TCP, UDP, Named Pipes）

**缺点：**
- 工作量大（4-6 周）
- 需要深入理解 Winsock2 API
- 维护成本高

**实现步骤：**

#### Phase 1: Windows Socket 抽象层（1 周）
```rust
// src/devices/src/virtio/vsock/tsi_windows/socket_wrapper.rs

pub struct WindowsSocket {
    socket: SOCKET,
    family: AddressFamily,
    sock_type: SockType,
}

impl WindowsSocket {
    pub fn new(family: AddressFamily, sock_type: SockType) -> io::Result<Self>;
    pub fn connect(&self, addr: &SocketAddr) -> io::Result<()>;
    pub fn bind(&self, addr: &SocketAddr) -> io::Result<()>;
    pub fn listen(&self, backlog: i32) -> io::Result<()>;
    pub fn accept(&self) -> io::Result<(Self, SocketAddr)>;
    pub fn send(&self, buf: &[u8]) -> io::Result<usize>;
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize>;
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;
}
```

#### Phase 2: TSI Stream Proxy（1-2 周）
```rust
// src/devices/src/virtio/vsock/tsi_windows/stream_proxy.rs

pub struct TsiStreamProxyWindows {
    id: u64,
    cid: u64,
    family: AddressFamily,
    local_port: u32,
    peer_port: u32,
    socket: WindowsSocket,
    status: ProxyStatus,
    // ... 其他字段
}

impl TsiStreamProxyWindows {
    pub fn new(...) -> Result<Self, ProxyError>;
    pub fn process_connect(&mut self, req: TsiConnectReq) -> Result<(), ProxyError>;
    pub fn process_listen(&mut self, req: TsiListenReq) -> Result<(), ProxyError>;
    pub fn process_accept(&mut self, req: TsiAcceptReq) -> Result<(), ProxyError>;
    // ... 其他方法
}
```

#### Phase 3: TSI DGRAM Proxy（1 周）
```rust
// src/devices/src/virtio/vsock/tsi_windows/dgram_proxy.rs

pub struct TsiDgramProxyWindows {
    id: u64,
    cid: u64,
    family: AddressFamily,
    local_port: u32,
    socket: WindowsSocket,
    // ... 其他字段
}
```

#### Phase 4: 集成和测试（1-2 周）
- 修改 `muxer.rs` 以支持 Windows TSI proxy
- 添加 Windows 特定的 TSI 测试
- 端到端测试和调试

### 方案 B：最小 TSI 实现（快速方案）

**范围：**
- 仅支持 TCP (AF_INET, AF_INET6)
- 不支持 Unix domain sockets（Windows 用 Named Pipes 替代）
- 简化的错误处理

**优点：**
- 工作量小（2-3 周）
- 满足大多数用例（TCP 网络）

**缺点：**
- 功能不完整
- 不支持 Unix sockets

### 方案 C：使用 virtio-net（当前方案）

**优点：**
- 已经实现并工作
- 无需额外开发
- 标准 virtio 设备，兼容性好

**缺点：**
- 不如 TSI 透明
- 需要配置网络后端
- 性能略低于 TSI

## 技术细节

### Windows Socket API 映射

| POSIX API | Windows API | 说明 |
|-----------|-------------|------|
| `socket()` | `WSASocket()` | 创建 socket |
| `connect()` | `connect()` | 相同 |
| `bind()` | `bind()` | 相同 |
| `listen()` | `listen()` | 相同 |
| `accept()` | `accept()` | 相同 |
| `send()` | `send()` | 相同 |
| `recv()` | `recv()` | 相同 |
| `fcntl(O_NONBLOCK)` | `ioctlsocket(FIONBIO)` | 设置非阻塞 |
| `close()` | `closesocket()` | 关闭 socket |
| `AF_UNIX` | Named Pipes | 完全不同 |

### Named Pipes vs Unix Sockets

**Unix Sockets (Linux/macOS):**
```rust
let socket = socket(AF_UNIX, SOCK_STREAM, 0);
bind(socket, "/tmp/mysocket");
listen(socket, 5);
```

**Named Pipes (Windows):**
```rust
let pipe = CreateNamedPipeA(
    "\\\\.\\pipe\\mysocket",
    PIPE_ACCESS_DUPLEX,
    PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES,
    4096, 4096, 0, None
);
ConnectNamedPipe(pipe, None);
```

**差异：**
- API 完全不同
- 语义略有不同（Named Pipes 更像 FIFO）
- 需要单独的实现路径

## 建议

### 短期（立即）

1. **评估需求**：
   - a3s box 是否真正需要 TSI？
   - virtio-net 是否足够？
   - 哪些应用场景依赖 TSI？

2. **如果不需要 TSI**：
   - 使用当前的 virtio-net 实现
   - Windows 后端已经 95% 就绪
   - 可以立即投入生产

### 中期（如果需要 TSI）

3. **选择实现方案**：
   - 方案 A（完整）：如果需要完整功能对等
   - 方案 B（最小）：如果只需要 TCP 支持
   - 方案 C（virtio-net）：如果可以接受非透明网络

4. **分阶段实现**：
   - Phase 1: TCP only (2 周)
   - Phase 2: UDP support (1 周)
   - Phase 3: Named Pipes (1 周)
   - Phase 4: 优化和测试 (1 周)

### 长期

5. **维护和优化**：
   - 持续测试和 bug 修复
   - 性能优化
   - 与 Linux/macOS 版本保持同步

## 风险评估

| 风险 | 可能性 | 影响 | 缓解措施 |
|------|--------|------|----------|
| Winsock2 API 复杂性 | 中 | 高 | 充分的原型验证 |
| Named Pipes 语义差异 | 高 | 中 | 文档化限制 |
| 性能问题 | 低 | 中 | 性能测试和优化 |
| 维护成本 | 中 | 中 | 良好的代码结构 |

## 结论

**TSI Windows 实现是可行的，但需要权衡：**

1. **如果 a3s box 不依赖 TSI**：
   - ✅ 使用 virtio-net（当前方案）
   - ✅ Windows 后端已经生产就绪（95%）
   - ✅ 可以立即部署

2. **如果 a3s box 必须有 TSI**：
   - ⚠️ 需要 4-6 周开发时间
   - ⚠️ 建议先实现 TCP only（2-3 周）
   - ⚠️ 然后根据需求扩展

3. **推荐行动**：
   - **立即**：与 a3s box 团队确认 TSI 是否必需
   - **如果必需**：启动 Phase 1（TCP only）
   - **如果不必需**：使用当前 virtio-net 方案

---

*评估日期：2026-03-05*
*评估人：Claude Sonnet 4.6*
