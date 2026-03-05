# Virtio-vsock DGRAM Implementation on Windows

## Overview

This document describes the implementation of DGRAM (datagram/connectionless) support for virtio-vsock on Windows, completing the P2 feature set for the Windows backend.

## Background

Virtio-vsock supports two socket types:
- **STREAM (type 1)**: Connection-oriented, reliable, ordered (like TCP)
- **DGRAM (type 3)**: Connectionless, unreliable, unordered (like UDP)

Prior to this implementation, the Windows backend only supported STREAM sockets via TCP and Named Pipes. DGRAM support enables connectionless communication scenarios.

## Architecture

### Data Structures

```rust
pub struct Vsock {
    // ... existing fields ...
    streams: HashMap<u32, StreamState>,        // STREAM sockets
    dgram_sockets: HashMap<u32, UdpSocket>,    // DGRAM sockets (NEW)
    // ... other fields ...
}
```

### Key Components

1. **DGRAM Socket Management**
   - `dgram_sockets: HashMap<u32, UdpSocket>` maps guest port → UDP socket
   - Sockets are created on-demand when first DGRAM packet is sent
   - Each socket is bound to `0.0.0.0:0` (any local address/port)

2. **TX Path (Guest → Host)**
   - Guest sends DGRAM packet via `VSOCK_OP_RW` with `VSOCK_TYPE_DGRAM`
   - VMM creates UDP socket if not exists
   - VMM sends datagram to mapped host port via `UdpSocket::send_to()`

3. **RX Path (Host → Guest)**
   - `harvest_dgram_reads()` polls all DGRAM sockets
   - Receives datagrams via `UdpSocket::recv_from()`
   - Constructs vsock header with `VSOCK_TYPE_DGRAM`
   - Queues packet to guest RX queue

## Implementation Details

### Feature Advertisement

```rust
const AVAIL_FEATURES: u64 = (1 << VIRTIO_F_VERSION_1 as u64)
    | (1 << VIRTIO_F_IN_ORDER as u64)
    | (1 << VIRTIO_VSOCK_F_DGRAM as u64);  // Bit 3
```

### TX Processing (VSOCK_OP_RW)

```rust
if pkt_type == VSOCK_TYPE_DGRAM {
    // Create socket on first use
    if !self.dgram_sockets.contains_key(&src_port) {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_nonblocking(true)?;
        self.dgram_sockets.insert(src_port, socket);
    }

    // Send datagram to host
    if let Some(socket) = self.dgram_sockets.get(&src_port) {
        if let Some(addr) = self.host_socket_addr(dst_port) {
            socket.send_to(&payload, addr)?;
        }
    }
}
```

### RX Processing (harvest_dgram_reads)

```rust
fn harvest_dgram_reads(&mut self) {
    for (guest_port, socket) in &self.dgram_sockets {
        let mut rx_buf = [0u8; 4096];
        match socket.recv_from(&mut rx_buf) {
            Ok((n, peer_addr)) => {
                // Construct vsock header
                let mut hdr = [0u8; 44];
                Self::set_u64(&mut hdr, 0, VSOCK_HOST_CID);
                Self::set_u64(&mut hdr, 8, self.cid);
                Self::set_u32(&mut hdr, 16, peer_addr.port() as u32);
                Self::set_u32(&mut hdr, 20, guest_port);
                Self::set_u32(&mut hdr, 24, n as u32);
                Self::set_u16(&mut hdr, 28, VSOCK_TYPE_DGRAM);
                Self::set_u16(&mut hdr, 30, VSOCK_OP_RW);

                self.queue_response(&hdr, VSOCK_OP_RW, rx_buf[..n].to_vec());
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => {}
        }
    }
}
```

## Differences from STREAM

| Aspect | STREAM | DGRAM |
|--------|--------|-------|
| Connection | Requires REQUEST/RESPONSE handshake | No handshake |
| State | Maintains StreamState per connection | Stateless (socket per port) |
| Flow Control | Credit-based (buf_alloc, fwd_cnt, tx_cnt) | None |
| Backend | TCP or Named Pipe | UDP |
| Reliability | Guaranteed delivery, ordered | Best-effort, may be lost/reordered |
| Operations | REQUEST, RESPONSE, RW, CREDIT_UPDATE, SHUTDOWN, RST | RW only |

## Testing

### Smoke Test

```rust
#[test]
fn test_whpx_vsock_dgram_feature() {
    let vsock = Vsock::new(3, None, None, Default::default()).unwrap();

    // Verify DGRAM feature is advertised
    let features = vsock.avail_features();
    assert_ne!(features & (1 << 3), 0, "VIRTIO_VSOCK_F_DGRAM not advertised");
}
```

### Test Results

```
running 54 tests
test windows::vstate::tests::test_whpx_vsock_dgram_feature ... ok
test windows::vstate::tests::test_whpx_vsock_init_smoke ... ok
test windows::vstate::tests::test_whpx_vsock_tx_smoke ... ok
test result: ok. 44 passed; 0 failed; 10 ignored; 0 measured
```

## Limitations

1. **Port Mapping Heuristic**: RX path uses peer UDP port as guest dst_port. This may not match the original guest port if NAT is involved.

2. **No Reverse Mapping**: The implementation doesn't maintain a reverse mapping from host UDP ports to guest ports, which could cause issues in complex scenarios.

3. **UDP Only**: DGRAM support is limited to UDP. Named Pipe DGRAM is not implemented (Windows Named Pipes don't support datagram mode).

4. **No Fragmentation**: Large datagrams (>4096 bytes) are not supported. UDP fragmentation is handled by the network stack.

## Future Improvements

1. **Port Mapping Table**: Maintain bidirectional mapping between guest ports and host UDP ports for accurate RX routing.

2. **Socket Cleanup**: Implement timeout-based cleanup for idle DGRAM sockets to prevent resource leaks.

3. **Error Handling**: Improve error handling for socket creation and I/O failures.

4. **Metrics**: Add counters for DGRAM packets sent/received, errors, etc.

## References

- [Virtio Specification - vsock Device](https://docs.oasis-open.org/virtio/virtio/v1.2/csd01/virtio-v1.2-csd01.html#x1-4050008)
- [Linux vsock DGRAM implementation](https://github.com/torvalds/linux/blob/master/net/vmw_vsock/af_vsock.c)
- Windows UDP Socket API: `std::net::UdpSocket`

---

*Implementation Date: 2026-03-05*
*Commit: e7700cc*
