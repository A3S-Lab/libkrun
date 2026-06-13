# vsock restore — diagnosis (workflow wf_9250ff73)

## Empirical ground truth
Restored guest vsock TX = 0 (no OP_RESPONSE/RST/timesync); host muxer alive (new_reverse/push_op_request/IRQ on port 4089); RNG+console queues work on the same VM. pop() returns None → host OP_REQUEST parked in MuxerRxQ, never delivered (process_stream_rx ENTER=0).

## Synthesis plan

This is the smoking gun that resolves the disagreement decisively against the comment's optimistic claim. Line 384 claims "vCPUs are paused, so the rings are quiescent and the saved indices are self-consistent." But pause only stops the **vCPUs** — it does NOT stop the **host muxer thread** (`MuxerThread`) or the **device event handlers**, which run on separate threads and continue to `pop()`/`add_used()`/mutate `next_avail`/`next_used` on the queues. More importantly for the synthesis: pausing vCPUs guarantees the *guest driver* state in RAM is frozen, and `save_virtio_states` reads the device's `next_avail`/`next_used`. If no in-flight RX/TX activity was happening at snapshot, these are consistent.

I now have all the evidence needed. The empirical ground truth (Lens 1) plus the code confirms the mechanism. Let me write the synthesis.

## SYNTHESIS

### 1. ROOT CAUSE

The guest exec server is silent after restore because **the host's OP_REQUEST is never delivered into a guest-visible RX descriptor**, so the guest's vsock driver never sees an inbound connection, never `accept()`s on port 4089, and therefore never produces any TX traffic. The "zero guest TX" in the packet trace is a *symptom*, not the fault: vsock TX is guest-initiated and there is simply nothing for the guest to send until it observes the inbound RX packet. The fault is on the **host→guest RX delivery path**, and it is an **RX virtqueue index desync introduced by the restore sequence**, not a host-muxer, connection-table, credit, or persistent-connection problem.

Concretely (this is where Lens 2's mechanism is correct and is confirmed by Lens 1's ground truth):

- On restore, `MmioTransport::restore_state` (mmio.rs:291) re-applies the saved `QueueState` (`next_avail`/`next_used`) to the freshly-built queues, then calls `device.activate()`. The ioeventfds and irqfd are correctly (re)registered by `register_mmio_device` (kvm/mmio.rs:153,157) on the freshly-built device tree, and `handle_activate_event` (event_handler.rs:77-120) re-registers the RXQ/TXQ eventfds into the EventManager when the pending `activate_evt` is pumped. So all *plumbing* is intact — which is exactly why RNG/console (guest-initiated, self-contained) work.
- When `a3s-box exec` fires, the host muxer does `new_reverse` → `push_op_request` → `push_packet` (muxer.rs:90-115), which calls `queue.pop(mem)`. `pop` computes available count as `avail_idx(read from guest RAM) − next_avail(restored device value)` (queue.rs:489). **If these two values disagree by even one, the OP_REQUEST lands in the wrong slot or is parked only in the software `MuxerRxQ` (muxer.rs:108) with no guest buffer to land in.** Either way the guest's RX driver, whose `last_seen` indices were captured from RAM, never observes a new used entry on port 4089. The IRQ is raised (host side is alive, as the trace shows), but the guest finds nothing new in the ring and does nothing.

Why the desync exists despite vCPU-pause-before-snapshot: `pause_vcpus()` (lib.rs:344) freezes only the **guest driver** side (RAM). It does **not** stop the **host `MuxerThread`** (muxer_thread.rs) or the device's own event handlers, which run on independent threads and keep calling `pop()`/`add_used()` and mutating `next_avail`/`next_used` on the very same RX/TX queues. The boot-time exec readiness probe (`wait_for_exec_ready` / `probe_exec_ready_once`, ready.rs:130) drives at least one complete host→guest reverse connection through the RX queue immediately before snapshot. The comment at lib.rs:384 ("vCPUs are paused, so the rings are quiescent and the saved indices are self-consistent") is **false for vsock**: the muxer thread can advance the device-side RX indices after the vCPUs are frozen but before/while `save_virtio_states` reads them, so the saved device `next_avail`/`next_used` no longer match the guest driver's `last_seen` indices frozen in RAM.

Eliminated hypotheses (high confidence, all three lenses + code agree):
- **Not** a stale persistent host exec connection. `ExecClient::connect` drops its probe stream (exec.rs:44-56); every exec/heartbeat opens a fresh `UnixStream`; each is a fresh stateless `new_reverse`. Lens 3 is right that there's nothing to preserve.
- **Not** missing muxer connection-table / local-port / epoll / unix-backend restore. Those are correctly recreated empty on `activate()`, and that's correct because every exec is a fresh reverse connection.
- **Not** credit/flow-control. `new_reverse` starts with zero peer credit and gets it from the guest's OP_RESPONSE (unix.rs:500-517); the blocker is the missing OP_RESPONSE, which is upstream of credit.
- **Not** notification suppression on the host→guest IRQ. `signal_used_queue` is unconditional on the push path (mmio.rs:208) and the trace confirms the IRQ fires.

### 2. THE FIX

**Simplest robust primary fix — snapshot only when the vsock RX/TX queues are genuinely quiescent, by quiescing the muxer and draining the exec connection before capturing device state.** This removes the desync at its source rather than trying to repair it on restore.

Two coordinated changes:

**(a) Box runtime — drop the exec control connection and let it reap before snapshotting.**
File: `crates/box/src/runtime/src/grpc/` (the `wait_for_exec_ready` / `self.exec_client` owner and the code that sends `snapshot <file>` over `KRUN_SNAPSHOT_SOCK`).
Before sending the snapshot command: ensure no exec/heartbeat is in flight, then wait out the 5s `ReaperThread` window (reaper.rs:10) — or better, add an explicit drain — so the host `MuxerThread` has finished tearing down the boot-probe reverse connection and is idle. The goal: at the instant `save_virtio_states` runs, no muxer activity is mutating the RX/TX queues.

**(b) libkrun — quiesce the muxer inside the snapshot critical section.**
File: `src/vmm/src/lib.rs::snapshot_to` (line 366), right after `pause_vcpus()` returns and before `save_virtio_states()` (line 385).
Add a `Vsock::quiesce_for_snapshot()` that pauses/blocks the `MuxerThread` (stop it from touching the queue) and re-reads the device's `next_avail`/`next_used` so the saved `QueueState` is captured atomically with the frozen guest RAM. Concretely, take the same `queue_rx`/`queue_tx` mutexes the muxer uses (muxer.rs:97 `queue_mutex.lock()`) across `save_virtio_states`, guaranteeing no `pop`/`add_used` interleaves with the device-state read.

**Fallback / defense-in-depth (Lens 2 #1) — resync RX `next_avail`/`next_used` from guest RAM on restore.** If quiescing at snapshot proves insufficient, in the vsock restore path (`Vsock::activate` restore branch, device.rs:256, reached via mmio.rs:312) add a `Queue::resync_from_guest_mem(mem)` helper (next to `restore_state`, queue.rs:409) that, for the RX queue, recomputes `next_avail` against the guest-written `avail_idx` and sets `next_used` from the in-RAM `used_ring.idx`. The RAM image is the single source of truth at restore; this makes the very first post-restore `push_op_request` land in a descriptor the guest actually observes. Pair it with a single post-restore RX kick (`muxer.signal_rx_ready()` + `process_stream_rx()`).

Avoid Lens 1's "re-kick the guest TX queue" framing as the primary fix — it treats the symptom. The guest has no TX to flush; fixing RX delivery makes TX follow automatically.

### 3. DECISIVE EXPERIMENT (before/after)

Confirm the index-desync hypothesis directly, then confirm the fix.

**Confirm root cause:** At snapshot time, log the device-side `next_avail`/`next_used` for the RX queue (in `save_virtio_states`) and, on restore, log the guest's `avail_idx` (read from RAM in `queue.pop`) at the first `push_op_request`. Predicted broken signature: `pop()` returns `None` (because `next_avail == avail_idx`) so the OP_REQUEST is parked in `MuxerRxQ` only — i.e. `process_stream_rx ENTER = 0` and `RX host->guest(delivered) = 0` exactly as Lens 1 measured, with the index pair mismatched against a clean (non-restored) boot.

**Confirm fix:** Re-run experiment (a) baseline restore + `a3s-box exec vrst -- echo hi`. Pass criterion = the packet trace shows `VTRACE TX guest->host op=RESPONSE src_port=4089` within ms of the host OP_REQUEST and `exec` returns rc=0 (not 124). Secondary assertion: `handle_txq_event` "TX queue event" count > 0 and `process_stream_rx ENTER > 0` on the restored box's shim.stderr.log.

### 4. SINGLE HIGHEST-LEVERAGE STEP NOW

Implement fix (b): in `src/vmm/src/lib.rs::snapshot_to`, hold the vsock `queue_rx`/`queue_tx` mutexes (the same `Arc<Mutex<VirtQueue>>` the muxer locks at muxer.rs:97) across the `save_virtio_states()` call, after `pause_vcpus()`. This is a small, localized, low-risk change that makes the saved device queue indices atomic with the frozen guest RAM and directly invalidates the false "rings are quiescent" assumption at lib.rs:384. Run the decisive experiment above; if a residual one-descriptor skew remains, layer on the restore-side `resync_from_guest_mem` fallback.

Key file:function references for the implementer:
- `src/vmm/src/lib.rs:366` `Vmm::snapshot_to` — add muxer quiesce / queue-lock around `save_virtio_states` (line 385).
- `src/devices/src/virtio/vsock/muxer.rs:90` `push_packet` / `97` `queue_mutex.lock()` — the queue mutex to share.
- `src/devices/src/virtio/queue.rs:489` `pop` len computation — where the desync manifests; `409` `restore_state` — add `resync_from_guest_mem` next to it for the fallback.
- `src/devices/src/virtio/vsock/device.rs:256` `Vsock::activate` (restore branch) — where the fallback resync + RX kick would go.
- `crates/box/src/runtime/src/grpc/` — drop/reap the exec connection before sending `snapshot` over `KRUN_SNAPSHOT_SOCK`.