# Snapshot-fork — Phase A/B status

## RESOLVED (2026-06-13): restored guest now runs correctly

The deterministic `0x24470e0` restore fault was **NOT** virtio device-ring state
(that was a red herring — the fault was byte-identical with/without device-state
restore, with 1/2 vCPUs, with `nosmp`, and with `clocksource=tsc no-kvmclock`).

**Real root cause:** the libkrunfw **kernel-image region `[0x1000000, 0x2450000)`
(~21 MB)** was dropped from the snapshot. `arch_memory_regions` punches a hole there;
`Payload::KernelMmap` (builder.rs ~2983) inserts that region via
`guest_mem.insert_region(build_raw(...))` over libkrunfw host pages **after**
`create_guest_memory_regions`, so it was never in `arch_mem_regions`, never file-backed,
never in `mem_layout`. The guest's IDT / early page tables / kernel `.data` live in that
region, so on restore the hole had no KVM slot → the first interrupt reads the IDT at
GPA `0x2447000` (the `#PF` gate at `0x2447000 + 0xe0 = 0x24470e0`) → `DELIVERY_EV`.

**Fix** (commits `cfec086` + `b823f27`): in snapshot mode `Payload::KernelMmap` copies
the kernel into the snapshot RAM file, maps it `MAP_SHARED` (capturing the guest's
runtime writes), and appends it to `MEM_BACKING.layout`; the restore branch sorts
`mem_layout` by guest address (`from_regions` requires sorted) and maps all 4 regions
`MAP_PRIVATE`. **Verified on KVM:** restored guest survives 10 s+ (was <1 s), zero
`INTERNAL_ERROR`, zero ERROR lines; `ram.img` grew by exactly `0x1450000`.

Phase B virtio device-state save/restore (below) is **implemented and kept** — it is
needed once the restored guest exercises virtio I/O (virtio-fs / vsock backends), but it
was not the cause of the boot-time fault.

## Status after Phase A (branch `feat/snapshot-restore`)

Phase A (RAM + CPU snapshot/restore) is **plumbing-complete and verified**:

- **Snapshot** (`KRUN_SNAPSHOT_MEM_FILE` + `KRUN_SNAPSHOT_SOCK`): guest RAM is
  file-backed `MAP_SHARED`; on the `snapshot <path>` socket command the vCPUs are
  paused, KVM VM + per-vCPU state is saved (`Vm::save_state` / `Vcpu::save_state`),
  the RAM file is fsync'd, and a bincode `SnapshotState` is written. Produces a
  1.09 GB `ram.img` + ~10–19 KB `state.bin`, reply `ok`.
- **Restore** (`KRUN_RESTORE_FROM` + `KRUN_SNAPSHOT_MEM_FILE`): each RAM region is
  remapped `MAP_PRIVATE` (kernel page-level CoW) at the saved layout; kernel/cmdline/
  firmware load and boot vCPU config are skipped; `Vm::restore_state` +
  `Vcpu::restore_state` re-apply KVM state before `start_vcpus`.
- **CPU-state restore is byte-perfect** — verified save-time vs restore-time:
  `rip`, `rsp`, `cr0/cr3/cr4`, `efer`, `cs.base/sel` identical on every vCPU.

### The remaining blocker (root cause, confirmed)

The restored guest resumes at the correct RIP, runs briefly, then takes a
**deterministic wild jump**: `KVM_EXIT_INTERNAL_ERROR` suberror 3 (DELIVERY_EV),
a #PF on instruction-fetch (error `0x31` = present + instruction-fetch) at CR2
`0x24470e0` — the *same address* every run, with 1 or 2 vCPUs.

- Not the clock: forcing `clocksource=tsc tsc=reliable no-kvmclock` on the template
  boot did **not** fix it.
- Not missing RAM: `memory_init` registers exactly the 3 file-backed regions; the
  kernel image (at R1 base `0x2450000`) is captured. Region map:
  `R0 gpa=0x0 size=0x1000000`, `R1 gpa=0x2450000 size=0x20000000`,
  `R2 gpa=0x100000000 size=0x20000000`; save and restore layouts are identical.

Root cause: **virtio device-ring state is lost on restore.** On restore the host
re-creates every virtio device fresh — `MmioTransport.device_status = INIT`, queues
at `next_avail/next_used = 0`, device not activated — while the guest's restored RAM
has the drivers at `DRIVER_OK` with the ring indices advanced. The guest driver, on
its next completion/interrupt, reads a stale used-ring entry and branches through a
garbage pointer → the deterministic wild jump.

## Phase B work surface

Goal: after building the device manager but **before** `start_vcpus`, re-apply the
saved virtio state so each fresh host device resumes at the guest's indices and is
re-activated without the guest replaying the setup MMIO writes.

### State to serialize (add to `SnapshotState`)

Per MMIO virtio device, keyed by MMIO base address (stable across boots for the same
device topology):

- From `MmioTransport` (`src/devices/src/virtio/mmio.rs:96`):
  `device_status`, `acked_features_select`, `queue_select`.
- From the `VirtioDevice` (`src/devices/src/virtio/device.rs`):
  `acked_features` (the negotiated feature bits) — needs an accessor.
- Per `Queue` (`src/devices/src/virtio/queue.rs:324`):
  `size`, `ready`, `desc_table`, `avail_ring`, `used_ring`,
  `next_avail`, `next_used`, `event_idx_enabled`, `num_added`.
  (Several are `pub(crate)`; add a `QueueState` getter/setter or make the snapshot
  code part of the `devices` crate.)

### Restore sequence (in `build_microvm`, restore branch, before `start_vcpus`)

For each MMIO device in the saved set:
1. `set_acked_features(saved.acked_features)` on the device.
2. Overwrite the device's `queues` with the saved `QueueState`s.
3. Set `MmioTransport.{device_status, acked_features_select, queue_select}`.
4. If saved `device_status` has `DRIVER_OK`, call `device.activate(mem, interrupt)`
   to respawn the worker thread + register ioeventfd/irqfd (mirrors the
   `DRIVER_OK` path at `mmio.rs:344`), bypassing the guest MMIO handshake.

### Devices used by an idle alpine box (minimum set to get a clean restore)

- **virtio-fs** (root `rootfstype=virtiofs`) — `src/devices/src/virtio/fs/`
- **vsock** (exec/control) — `src/devices/src/virtio/vsock/`
- **console** (`console=hvc0`) — `src/devices/src/virtio/console/`
- **rng**, **balloon** if attached — `rng/`, `balloon/`

Each device needs: an `acked_features` accessor, a `queues_mut()` / state setter, and
an idempotent re-activation path. virtio-fs and vsock are the hard ones (worker
threads + backend fds); console/rng are simpler and a good first target to prove the
re-activation sequence end-to-end.

### Quiescing requirement

Snapshot must be taken at a device-quiescent point (no in-flight requests, rings
drained) so the saved indices are self-consistent. An **idle deferred-main template**
(`BOX_DEFERRED_MAIN=1`, queues empty) is the intended snapshot subject — snapshot the
template, fork children from it. Verify the avail/used indices are equal (no pending)
at snapshot time before trusting the restore.

## Path to the 20ms / 100-VM target (after correctness)

Correct single-VM restore is expected at ~10–150 ms first. To approach 100-in-20 ms:
1. **Strip the restore path** — skip device *construction* cost; pre-build a device
   template and clone, not re-parse config per VM.
2. **Parallel restore** across cores (the MAP_PRIVATE fault-in is lazy, so per-VM
   wall cost is dominated by KVM ioctls + device worker spawn).
3. **Shared RAM template page cache** — all forks `MAP_PRIVATE` the same warm file;
   only divergent pages cost memory.
4. Measure honestly and report the real per-VM and 100-VM numbers at each step.
