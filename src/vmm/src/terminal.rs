#[cfg(not(target_os = "windows"))]
use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg, Termios};

#[cfg(not(target_os = "windows"))]
pub struct TerminalMode(Termios);

#[cfg(target_os = "windows")]
#[must_use]
#[allow(dead_code)]
pub struct TerminalMode;

#[cfg(not(target_os = "windows"))]
pub fn term_set_raw_mode(
    term: std::os::fd::BorrowedFd,
    handle_signals_by_terminal: bool,
) -> Result<TerminalMode, nix::Error> {
    let mut termios = tcgetattr(term)?;
    let old_state = termios.clone();

    let mut mask = LocalFlags::ECHO | LocalFlags::ICANON;
    if !handle_signals_by_terminal {
        mask |= LocalFlags::ISIG
    }

    termios.local_flags &= !mask;
    tcsetattr(term, SetArg::TCSANOW, &termios)?;
    Ok(TerminalMode(old_state))
}

#[cfg(target_os = "windows")]
#[allow(dead_code)]
pub fn term_set_raw_mode(
    _term: i32,
    _handle_signals_by_terminal: bool,
) -> Result<TerminalMode, std::io::Error> {
    Ok(TerminalMode)
}

#[cfg(not(target_os = "windows"))]
pub fn term_restore_mode(
    term: std::os::fd::BorrowedFd,
    restore: &TerminalMode,
) -> Result<(), nix::Error> {
    tcsetattr(term, SetArg::TCSANOW, &restore.0)
}

#[cfg(target_os = "windows")]
#[allow(dead_code)]
pub fn term_restore_mode(_term: i32, _restore: &TerminalMode) -> Result<(), std::io::Error> {
    Ok(())
}
