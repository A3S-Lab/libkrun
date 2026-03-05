# TSI Windows Integration Plan (Phase 5)

## Status: Phase 1-4 Complete, Phase 5 In Progress

### Completed Phases (1-4)

#### Phase 1: Windows Socket Abstraction ✅
- `socket_wrapper.rs` (~400 lines)
- WindowsSocket wrapper around Winsock2 APIs
- Address family conversion (Linux ↔ Windows)
- Non-blocking I/O support
- Unit tests passing

#### Phase 2: TCP Stream Proxy ✅
- `stream_proxy.rs` (~300 lines)
- TsiStreamProxyWindows for TCP connections
- State machine: Init → Connecting → Connected / Listening
- connect, listen, accept, send/recv operations
- Unit tests passing

#### Phase 3: UDP DGRAM Proxy ✅
- `dgram_proxy.rs` (~220 lines)
- TsiDgramProxyWindows for UDP sockets
- bind, sendto, recvfrom operations
- Remote address caching
- Unit tests passing

#### Phase 4: Named Pipes Proxy ✅
- `pipe_proxy.rs` (~230 lines)
- TsiPipeProxyWindows for Windows Named Pipes
- Server mode: CreateNamedPipe + ConnectNamedPipe
- Client mode: CreateFileW
- send_data/recv_data for bidirectional communication
- Unit tests passing

### Phase 5: Integration with vsock muxer (In Progress)

#### Architecture Overview

The vsock muxer uses a trait-based design:
- `Proxy` trait defines the interface for all connection types
- `TsiStreamProxy` (Unix) implements Proxy for TCP/Unix sockets
- `TsiDgramProxy` (Unix) implements Proxy for UDP sockets

Windows needs equivalent implementations:
- `TsiStreamProxyWindowsWrapper` - wraps TsiStreamProxyWindows + TsiPipeProxyWindows
- `TsiDgramProxyWindowsWrapper` - wraps TsiDgramProxyWindows

#### Key Files to Modify

1. **tsi_stream_windows.rs** (new, ~800 lines estimated)
   - Implement `Proxy` trait for Windows TCP/Named Pipes
   - Handle vsock packet operations: connect, listen, accept, sendmsg
   - Credit-based flow control
   - Event-driven I/O via EventSet

2. **tsi_dgram_windows.rs** (new, ~600 lines estimated)
   - Implement `Proxy` trait for Windows UDP
   - Handle sendto/recvfrom with vsock packets
   - Address translation between guest and host

3. **muxer.rs** (modify)
   - Add Windows-specific proxy creation paths
   - Conditional compilation for Unix vs Windows

4. **mod.rs** (modify)
   - Export Windows TSI modules
   - Conditional compilation

#### Proxy Trait Methods to Implement

```rust
pub trait Proxy: Send + AsRawFd {
    fn id(&self) -> u64;
    fn status(&self) -> ProxyStatus;
    fn connect(&mut self, pkt: &VsockPacket, req: TsiConnectReq) -> ProxyUpdate;
    fn confirm_connect(&mut self, pkt: &VsockPacket) -> Option<ProxyUpdate>;
    fn getpeername(&mut self, pkt: &VsockPacket);
    fn sendmsg(&mut self, pkt: &VsockPacket) -> ProxyUpdate;
    fn sendto_addr(&mut self, req: TsiSendtoAddr) -> ProxyUpdate;
    fn sendto_data(&mut self, pkt: &VsockPacket);
    fn listen(&mut self, pkt: &VsockPacket, req: TsiListenReq,
              host_port_map: &Option<HashMap<u16, u16>>) -> ProxyUpdate;
    fn accept(&mut self, req: TsiAcceptReq) -> ProxyUpdate;
    fn update_peer_credit(&mut self, pkt: &VsockPacket) -> ProxyUpdate;
    fn push_op_request(&self);
    fn process_op_response(&mut self, pkt: &VsockPacket) -> ProxyUpdate;
    fn enqueue_accept(&mut self);
    fn push_accept_rsp(&self, result: i32);
    fn shutdown(&mut self, pkt: &VsockPacket);
    fn release(&mut self) -> ProxyUpdate;
    fn process_event(&mut self, evset: EventSet) -> ProxyUpdate;
}
```

#### Windows-Specific Challenges

1. **AsRawFd trait**
   - Unix-specific trait
   - Need Windows equivalent: AsRawHandle
   - May need to create adapter trait or use conditional compilation

2. **EventSet handling**
   - Unix epoll-based event system
   - Windows uses different I/O completion model
   - Need to map Windows events to EventSet

3. **Credit-based flow control**
   - vsock uses credit-based flow control to prevent buffer overflow
   - Need to track: rx_cnt, tx_cnt, peer_buf_alloc, peer_fwd_cnt
   - Must implement update_peer_credit() correctly

4. **Address translation**
   - Guest uses Linux address family constants (AF_INET=2, AF_INET6=10)
   - Windows uses different constants
   - Already handled in socket_wrapper.rs

5. **Named Pipe integration**
   - Unix domain sockets → Windows Named Pipes
   - Path translation: /path/to/socket → \\.\pipe\name
   - Already handled in pipe_proxy.rs

#### Implementation Strategy

**Option A: Full Integration (2-3 weeks)**
- Implement complete Proxy trait for Windows
- Full feature parity with Unix TSI
- Requires extensive testing

**Option B: Minimal Viable Integration (1 week)**
- Implement core methods only (connect, sendmsg, release)
- Stub out advanced features (listen/accept, credit updates)
- Get basic TCP working first

**Option C: Incremental Integration (recommended, 1.5 weeks)**
1. Day 1-2: Implement TsiStreamProxyWindowsWrapper skeleton
   - Basic Proxy trait implementation
   - connect() and sendmsg() only
2. Day 3-4: Add listen/accept support
   - Server-side functionality
3. Day 5-6: Add credit-based flow control
   - update_peer_credit(), proper buffer management
4. Day 7-8: Implement TsiDgramProxyWindowsWrapper
   - UDP support
5. Day 9-10: Testing and bug fixes
   - Integration tests
   - End-to-end validation

#### Testing Plan

1. **Unit tests** (already done for Phase 1-4)
   - Socket creation, bind, connect
   - Send/recv operations
   - State transitions

2. **Integration tests** (Phase 5)
   - Create vsock device with TSI enabled
   - Guest initiates TCP connection
   - Data transfer validation
   - Connection teardown

3. **End-to-end tests**
   - Full VM boot with TSI vsock
   - Guest application uses TSI to connect to host
   - Verify data integrity

#### Next Steps

1. **Immediate**: Decide on implementation strategy (A/B/C)
2. **Short-term**: Implement TsiStreamProxyWindowsWrapper skeleton
3. **Medium-term**: Complete Proxy trait implementation
4. **Long-term**: Full testing and documentation

#### Dependencies

- Phase 1-4 complete ✅
- utils::epoll Windows support (may need adaptation)
- EventManager Windows support (already done)

#### Estimated Completion

- Option A: 2-3 weeks
- Option B: 1 week
- Option C: 1.5 weeks (recommended)

## Current Status

- Phase 1-4: ✅ Complete (committed and pushed)
- Phase 5: 🚧 In Progress
  - Created tsi_stream_windows.rs skeleton
  - Need to complete Proxy trait implementation
  - Need to create tsi_dgram_windows.rs
  - Need to integrate with muxer.rs

## Files Created

- `src/devices/src/virtio/vsock/tsi_windows/socket_wrapper.rs` (400 lines)
- `src/devices/src/virtio/vsock/tsi_windows/stream_proxy.rs` (300 lines)
- `src/devices/src/virtio/vsock/tsi_windows/dgram_proxy.rs` (220 lines)
- `src/devices/src/virtio/vsock/tsi_windows/pipe_proxy.rs` (230 lines)
- `src/devices/src/virtio/vsock/tsi_windows/mod.rs` (15 lines)
- `src/devices/src/virtio/vsock/tsi_stream_windows.rs` (partial, ~100 lines)

Total: ~1,265 lines of new Windows TSI code (Phase 1-4 complete)
