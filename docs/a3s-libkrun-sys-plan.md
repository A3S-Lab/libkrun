# a3s-libkrun-sys 实施计划

将 libkrun Windows 后端打包成供 a3s-box 使用的 `a3s-libkrun-sys` Rust sys crate。

---

## 背景与约束

| 项目 | 现状 |
|------|------|
| `src/libkrun` | cdylib，编译为 `krun.dll` + `krun.lib`（MSVC 自动生成） |
| `krun-sys` | 使用 `pkg-config` + `bindgen` 运行时生成绑定，Windows 不可用 |
| `include/libkrun.h` | 包含 Unix 专用函数（unixstream/unixgram/tap/vsock-path），需要筛除 |
| Windows 可用 API | 从 `lib.rs` 的 `#[cfg]` 推断（见下文 Phase 1） |

---

## Phase 1：整理 Windows 公开 API 子集

**目标**：明确 `a3s-libkrun-sys` 暴露哪些函数，写入 `include/libkrun_windows.h`。

**Windows 可用函数**（来自 `lib.rs` cfg 分析）：

```
krun_set_log_level       krun_init_log
krun_create_ctx          krun_free_ctx
krun_set_vm_config
krun_set_root
krun_set_kernel          ← Windows 必须调用
krun_add_disk            ← Windows 版本（BlockWindowsConfig）
krun_add_net_tcp         ← Windows 专有（NetWindowsConfig）
krun_add_vsock           krun_add_vsock_port_windows  ← Windows 专有
krun_set_exec            krun_set_env
krun_set_workdir
krun_set_console_output
krun_disable_implicit_console  krun_disable_implicit_vsock
krun_add_virtio_console_default
krun_add_serial_console_default
krun_add_virtio_console_multiport
krun_add_console_port_tty  krun_add_console_port_inout
krun_set_kernel_console
krun_get_shutdown_eventfd
krun_get_max_vcpus
krun_start_enter
```

**不暴露**（Unix only，Windows 编译时未实现）：
- `krun_add_net_unixstream`, `krun_add_net_unixgram`, `krun_add_net_tap`
- `krun_add_vsock_port`, `krun_add_vsock_port2`（使用 Unix 文件路径）
- `krun_add_virtiofs`, `krun_add_virtiofs2`（virtiofs 在 Windows 需验证）
- `krun_setuid`, `krun_setgid`（uid_t/gid_t 在 Windows 无意义）
- `krun_set_rlimits`, `krun_set_nested_virt`, `krun_check_nested_virt`

**产出**：`include/libkrun_windows.h`（只包含上述函数，用 `#ifdef _WIN32` 保护）

**验证**：确认每个列出的函数在 `lib.rs` 中都有 `#[cfg(target_os = "windows")]` 实现或无条件实现。

---

## Phase 2：修复 `libkrun.h` Windows 兼容性

**问题**：当前 `include/libkrun.h` 包含 `<unistd.h>`，MSVC 不支持。

**修改** `include/libkrun.h`：

```c
#ifdef _WIN32
  #include <stdint.h>
  #include <stddef.h>
  #include <stdbool.h>
  typedef int32_t uid_t;
  typedef int32_t gid_t;
#else
  #include <unistd.h>
#endif
```

或直接在 `include/libkrun_windows.h` 里用 Windows 兼容 header，不改动原始文件。

---

## Phase 3：构建 `krun.dll` + `krun.lib`

**前提条件**：
```powershell
# 创建 fake init（build 时需要）
New-Item -ItemType File -Path "init/init" -Force
# 确认 WinHvPlatform 可用
```

**构建命令**：
```powershell
cargo build --release -p libkrun --target x86_64-pc-windows-msvc
```

**产物位置**：
```
target/x86_64-pc-windows-msvc/release/
  krun.dll          ← 运行时依赖（随 a3s-box.exe 分发）
  krun.dll.lib      ← MSVC import library（链接用）
```

**重命名**：
```powershell
Copy-Item target/x86_64-pc-windows-msvc/release/krun.dll.lib `
          target/x86_64-pc-windows-msvc/release/krun.lib
```

**验证**：
```powershell
dumpbin /exports target/.../krun.dll | findstr krun_
# 应列出所有 krun_* 符号
```

---

## Phase 4：生成 `bindings.rs`（一次性，检入 git）

**工具**：需要 LLVM/clang（通过 `winget install LLVM.LLVM`）

```powershell
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"

# 用 Phase 1 产出的 Windows 专用 header
bindgen include/libkrun_windows.h `
    --allowlist-function "krun_.*" `
    --allowlist-type "krun_.*" `
    --allowlist-var "KRUN_.*" `
    --no-prepend-enum-name `
    -- -target x86_64-pc-windows-msvc `
    -o a3s-libkrun-sys/src/bindings.rs
```

**或**使用 `cargo bindgen`（在 `a3s-libkrun-sys/build.rs` 中 feature-flag 可选）：
- 默认：直接 include 预生成的 `bindings.rs`（不需要 clang）
- `REGENERATE_BINDINGS=1`：重新运行 bindgen（开发者维护时用）

---

## Phase 5：创建 `a3s-libkrun-sys` crate

**目录结构**：
```
a3s-libkrun-sys/
├── Cargo.toml
├── build.rs
├── src/
│   ├── lib.rs
│   └── bindings.rs      ← Phase 4 生成，检入 git
└── prebuilt/
    └── x86_64-pc-windows-msvc/
        ├── krun.dll     ← Phase 3 构建，检入 git（LFS）或 CI artifact
        └── krun.lib     ← Phase 3 构建，检入 git
```

### `Cargo.toml`

```toml
[package]
name = "a3s-libkrun-sys"
version = "0.1.0"
edition = "2021"
links = "krun"

[build-dependencies]
# 无（bindings 已预生成）
```

### `build.rs`

```rust
use std::{env, path::PathBuf};

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    assert_eq!(target_os, "windows", "a3s-libkrun-sys only supports Windows");

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let triple = format!("{}-pc-windows-msvc", target_arch);

    // 1. 用户可通过 LIBKRUN_DIR 指定自定义构建目录（CI override）
    let lib_dir = env::var("LIBKRUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
                .join("prebuilt")
                .join(&triple)
        });

    assert!(
        lib_dir.join("krun.lib").exists(),
        "krun.lib not found in {}. Set LIBKRUN_DIR or provide prebuilt/{}",
        lib_dir.display(), triple
    );

    // 2. 链接指令
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=krun");
    println!("cargo:rustc-link-lib=WinHvPlatform");

    // 3. 把 krun.dll 位置暴露给上游 crate（通过 DEP_KRUN_DLL_DIR）
    println!("cargo:dll_dir={}", lib_dir.display());

    println!("cargo:rerun-if-env-changed=LIBKRUN_DIR");
    println!("cargo:rerun-if-changed=prebuilt/{}/krun.lib", triple);
}
```

### `src/lib.rs`

```rust
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

include!("bindings.rs");
```

---

## Phase 6：a3s-box 集成

### `a3s-box/Cargo.toml`

```toml
[target.'cfg(windows)'.dependencies]
a3s-libkrun-sys = { path = "../a3s-libkrun-sys" }
# 或 git:
a3s-libkrun-sys = { git = "https://your-org/a3s-libkrun-sys" }
```

### `a3s-box/build.rs`（DLL 部署）

Cargo 不自动复制 DLL 到输出目录，需要 build.rs 处理：

```rust
fn main() {
    #[cfg(windows)]
    {
        // DEP_KRUN_DLL_DIR 由 a3s-libkrun-sys/build.rs 通过 cargo:dll_dir= 暴露
        if let Ok(dll_dir) = std::env::var("DEP_KRUN_DLL_DIR") {
            let out = std::env::var("OUT_DIR").unwrap();
            // 找到最终二进制输出目录（OUT_DIR 的三级父目录）
            let bin_dir = std::path::PathBuf::from(&out)
                .ancestors().nth(3).unwrap().to_path_buf();
            let src = std::path::PathBuf::from(&dll_dir).join("krun.dll");
            let dst = bin_dir.join("krun.dll");
            if src.exists() && !dst.exists() {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
    }
}
```

### 使用示例

```rust
// a3s-box/src/main.rs
#[cfg(windows)]
use a3s_libkrun_sys::*;

fn start_vm() -> Result<()> {
    unsafe {
        let ctx = krun_create_ctx();
        assert!(ctx >= 0);
        krun_set_vm_config(ctx as u32, 2, 512);
        krun_set_kernel(ctx as u32, c"bzImage".as_ptr(), c"console=ttyS0".as_ptr());
        krun_add_disk(ctx as u32, c"root".as_ptr(), c"C:\\vms\\root.img".as_ptr(), false);
        krun_start_enter(ctx as u32);
    }
    Ok(())
}
```

---

## Phase 7：CI 自动化（GitHub Actions）

```yaml
# .github/workflows/build-windows-dll.yml
name: Build krun.dll

on:
  push:
    paths: ['src/**', 'include/**']
    branches: [main]

jobs:
  build:
    runs-on: windows-2022
    steps:
      - uses: actions/checkout@v4

      - name: Enable Windows Hypervisor Platform
        run: |
          Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform -NoRestart

      - name: Setup Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-pc-windows-msvc

      - name: Fake init
        run: New-Item -ItemType File -Path "init/init" -Force

      - name: Build krun.dll
        run: cargo build --release -p libkrun --target x86_64-pc-windows-msvc

      - name: Prepare artifacts
        run: |
          New-Item -ItemType Directory -Force -Path dist
          Copy-Item target/x86_64-pc-windows-msvc/release/krun.dll dist/
          Copy-Item target/x86_64-pc-windows-msvc/release/krun.dll.lib dist/krun.lib

      - name: Upload artifacts
        uses: actions/upload-artifact@v4
        with:
          name: krun-windows-x64
          path: dist/

      # 可选：自动更新 a3s-libkrun-sys 的 prebuilt/ 目录并提 PR
      - name: Update prebuilt in a3s-libkrun-sys
        if: github.ref == 'refs/heads/main'
        run: |
          # Copy to a3s-libkrun-sys repo checkout and open PR
          # ... 视 monorepo vs 分开 repo 而定
```

---

## 实施顺序与里程碑

```
Week 1
  [P1] 整理 Windows API 子集，写 libkrun_windows.h
  [P2] 修复 header Windows 兼容性（unistd.h 问题）
  [P3] 本地构建 krun.dll，验证导出符号

Week 2
  [P4] bindgen 生成 bindings.rs，手工检查类型映射
  [P5] 创建 a3s-libkrun-sys crate，本地测试链接

Week 3
  [P6] a3s-box 集成，端到端冒烟测试（krun_create_ctx → krun_start_enter）
  [P7] CI workflow，自动构建 + artifact 上传

Week 4
  验收测试：a3s-box 在干净 Windows 11 机器上能启动 VM
  发布 a3s-libkrun-sys v0.1.0 到私有 registry 或 git tag
```

---

## 风险与注意事项

| 风险 | 说明 | 缓解 |
|------|------|------|
| `krun.dll` 体积 | release 构建约 15~25 MB | 用 Git LFS 存 prebuilt/ |
| DLL 未找到（运行时） | `krun.dll` 不在 PATH/exe 同目录 | a3s-box build.rs 自动复制；CI 打包时验证 |
| WHPX 不可用 | 干净 VM 默认未启用 HypervisorPlatform | CI 和 README 说明先决条件 |
| API 签名漂移 | libkrun 更新函数签名后 bindings.rs 过时 | CI 在 libkrun 变更时触发 regenerate job |
| Windows header 兼容 | `uid_t`/`gid_t` 在 MSVC 未定义 | libkrun_windows.h 单独定义或用 `int32_t` 替代 |
