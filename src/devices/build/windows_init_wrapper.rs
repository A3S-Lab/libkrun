use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::OsStrExt;

const FALLBACK_INIT: &str = "/usr/sbin/init";
const LOG_PATH: &str = "/init-rust.log";
const DEFAULT_GUEST_INIT_STDOUT_PATH: &str = "/guest-init.stdout.log";
const DEFAULT_GUEST_INIT_STDERR_PATH: &str = "/guest-init.stderr.log";
const SYS_MEMFD_CREATE: isize = 319;
const MFD_EXEC: u32 = 0x0010;
static ORIGINAL_INIT_BINARY: &[u8] = include_bytes!(env!("LIBKRUN_WRAPPED_INIT_PATH"));

unsafe extern "C" {
    fn mount(
        source: *const i8,
        target: *const i8,
        fstype: *const i8,
        flags: usize,
        data: *const core::ffi::c_void,
    ) -> i32;
    fn umount(target: *const i8) -> i32;
    fn syscall(num: isize, ...) -> isize;
    fn fexecve(fd: i32, argv: *const *const i8, envp: *const *const i8) -> i32;
    fn fork() -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn close(fd: i32) -> i32;
    fn dup2(oldfd: i32, newfd: i32) -> i32;
}

fn log_line(msg: &str) {
    let _ = writeln!(std::io::stderr(), "{msg}");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(LOG_PATH) {
        let _ = writeln!(file, "{msg}");
    }
}

fn mount_fs(source: Option<&str>, target: &str, fstype: Option<&str>) -> std::io::Result<()> {
    let src = source.map(|s| std::ffi::CString::new(s).unwrap());
    let dst = std::ffi::CString::new(target).unwrap();
    let typ = fstype.map(|s| std::ffi::CString::new(s).unwrap());

    let rc = unsafe {
        mount(
            src.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            dst.as_ptr(),
            typ.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            0,
            std::ptr::null(),
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn parse_cmdline(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;

    for ch in raw.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }

    if !cur.is_empty() {
        out.push(cur);
    }

    out
}

fn extract_guest_env(tokens: &[String]) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();

    for token in tokens {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };

        if key.starts_with("KRUN_")
            || key.starts_with("BOX_")
            || key.starts_with("A3S_")
            || key.starts_with("RUST_")
            || key.starts_with("LD_")
        {
            env.insert(key.to_string(), value.to_string());
        }
    }

    env
}

fn read_cmdline() -> String {
    let mut raw = String::new();
    if let Ok(mut file) = fs::File::open("/proc/cmdline") {
        let _ = file.read_to_string(&mut raw);
        return raw;
    }

    let proc_target = std::ffi::CString::new("/proc").unwrap();
    let mounted_proc = mount_fs(Some("proc"), "/proc", Some("proc")).is_ok();
    if let Ok(mut file) = fs::File::open("/proc/cmdline") {
        let _ = file.read_to_string(&mut raw);
    }

    if mounted_proc {
        unsafe {
            let _ = umount(proc_target.as_ptr());
        }
    }

    raw
}

fn create_wrapped_init_memfd() -> std::io::Result<i32> {
    let name = std::ffi::CString::new("init.krun.real").unwrap();
    let fd = unsafe { syscall(SYS_MEMFD_CREATE, name.as_ptr(), MFD_EXEC) as i32 };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(ORIGINAL_INIT_BINARY)?;
    Ok(file.into_raw_fd())
}

fn open_append(path: &str) -> std::io::Result<fs::File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn redirect_fd(path: &str, target_fd: i32) -> std::io::Result<()> {
    let file = open_append(path)?;
    if unsafe { dup2(file.as_raw_fd(), target_fd) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn build_exec_argv() -> Vec<std::ffi::CString> {
    let mut argv = vec![std::ffi::CString::new("init.krun.real").unwrap()];
    for arg in std::env::args_os().skip(1) {
        argv.push(std::ffi::CString::new(arg.as_os_str().as_bytes()).unwrap());
    }
    argv
}

fn build_exec_env(env_map: &BTreeMap<String, String>) -> Vec<std::ffi::CString> {
    env_map
        .iter()
        .map(|(key, value)| std::ffi::CString::new(format!("{key}={value}")).unwrap())
        .collect()
}

fn main() {
    let raw = read_cmdline();
    log_line(&format!("cmdline={raw}"));

    let tokens = parse_cmdline(&raw);
    let mut env_map = extract_guest_env(&tokens);
    env_map
        .entry("KRUN_DEBUG_LOG".to_string())
        .or_insert_with(|| "/init.krun.log".to_string());
    if env_map.get("KRUN_WINDOWS_PREFER_BOX_EXEC").map(|value| value == "1") == Some(true)
        && env_map.contains_key("BOX_EXEC_EXEC")
    {
        if let Some(krun_init) = env_map.remove("KRUN_INIT") {
            log_line(&format!("prefer_box_exec_removed_krun_init={krun_init}"));
        } else {
            log_line("prefer_box_exec_without_krun_init");
        }
    }
    let guest_init_stdout_path = env_map
        .get("KRUN_GUEST_INIT_STDOUT_PATH")
        .cloned()
        .unwrap_or_else(|| DEFAULT_GUEST_INIT_STDOUT_PATH.to_string());
    let guest_init_stderr_path = env_map
        .get("KRUN_GUEST_INIT_STDERR_PATH")
        .cloned()
        .unwrap_or_else(|| DEFAULT_GUEST_INIT_STDERR_PATH.to_string());
    log_line(&format!("guest_init_stdout_path={guest_init_stdout_path}"));
    log_line(&format!("guest_init_stderr_path={guest_init_stderr_path}"));

    let requested_init = env_map
        .get("KRUN_INIT")
        .cloned()
        .unwrap_or_else(|| FALLBACK_INIT.to_string());
    log_line(&format!("requested_init={requested_init}"));
    let keep_guest_init_pid1 =
        env_map.get("KRUN_WINDOWS_KEEP_GUEST_INIT_PID1").map(|value| value == "1") == Some(true);
    if keep_guest_init_pid1 {
        log_line("keep_guest_init_pid1=1");
    }

    let memfd = match create_wrapped_init_memfd() {
        Ok(fd) => fd,
        Err(err) => {
            log_line(&format!("create_wrapped_init_memfd_failed={err}"));
            std::process::exit(126);
        }
    };

    let argv = build_exec_argv();
    let argv_ptrs: Vec<*const i8> = argv
        .iter()
        .map(|value| value.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let envp = build_exec_env(&env_map);
    let envp_ptrs: Vec<*const i8> = envp
        .iter()
        .map(|value| value.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    if keep_guest_init_pid1 {
        if let Err(err) = redirect_fd(&guest_init_stdout_path, 1) {
            log_line(&format!(
                "keep_guest_init_pid1_redirect_stdout_failed path={} err={}",
                guest_init_stdout_path, err
            ));
        } else {
            log_line(&format!(
                "keep_guest_init_pid1_redirect_stdout_ok path={guest_init_stdout_path}"
            ));
        }
        if let Err(err) = redirect_fd(&guest_init_stderr_path, 2) {
            log_line(&format!(
                "keep_guest_init_pid1_redirect_stderr_failed path={} err={}",
                guest_init_stderr_path, err
            ));
        } else {
            log_line(&format!(
                "keep_guest_init_pid1_redirect_stderr_ok path={guest_init_stderr_path}"
            ));
        }
        log_line("keep_guest_init_pid1_execing_wrapped_init");
        let rc = unsafe { fexecve(memfd, argv_ptrs.as_ptr(), envp_ptrs.as_ptr()) };
        let err = std::io::Error::last_os_error();
        log_line(&format!("keep_guest_init_pid1_fexecve_failed rc={rc} err={err}"));
        std::process::exit(127);
    }

    let child_pid = unsafe { fork() };
    if child_pid < 0 {
        log_line(&format!("fork_failed={}", std::io::Error::last_os_error()));
        unsafe {
            let _ = close(memfd);
        }
        std::process::exit(127);
    }

    if child_pid == 0 {
        if let Err(err) = redirect_fd(&guest_init_stdout_path, 1) {
            let _ = writeln!(
                std::io::stderr(),
                "init-rust redirect stdout failed path={} err={}",
                guest_init_stdout_path,
                err
            );
        }
        if let Err(err) = redirect_fd(&guest_init_stderr_path, 2) {
            let _ = writeln!(
                std::io::stderr(),
                "init-rust redirect stderr failed path={} err={}",
                guest_init_stderr_path,
                err
            );
        }
        let rc = unsafe { fexecve(memfd, argv_ptrs.as_ptr(), envp_ptrs.as_ptr()) };
        let err = std::io::Error::last_os_error();
        let _ = writeln!(std::io::stderr(), "init-rust fexecve failed rc={rc} err={err}");
        std::process::exit(127);
    }

    log_line(&format!("wrapped_init_spawned_pid={child_pid}"));
    unsafe {
        let _ = close(memfd);
    }

    let mut status = 0;
    let waited = unsafe { waitpid(child_pid, &mut status, 0) };
    if waited < 0 {
        log_line(&format!("wait_failed={}", std::io::Error::last_os_error()));
        std::process::exit(127);
    }

    if (status & 0x7f) == 0 {
        let code = (status >> 8) & 0xff;
        log_line(&format!("wrapped_init_exit_code={code}"));
        std::process::exit(code);
    }

    let signal = status & 0x7f;
    if signal != 0 {
        log_line(&format!("wrapped_init_exit_signal={signal}"));
        std::process::exit(128 + signal);
    }

    log_line("wrapped_init_exit_unknown");
    std::process::exit(1);
}
