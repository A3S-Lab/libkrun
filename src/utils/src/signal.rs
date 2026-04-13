use libc::c_int;
pub use vmm_sys_util::signal::*;

extern "C" {
    fn __libc_current_sigrtmin() -> c_int;
    fn __libc_current_sigrtmax() -> c_int;
}

pub fn sigrtmin() -> c_int {
    unsafe { __libc_current_sigrtmin() }
}

pub fn sigrtmax() -> c_int {
    unsafe { __libc_current_sigrtmax() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigrtmin_sigrtmax() {
        let min = sigrtmin();
        let max = sigrtmax();

        // RTMIN should be less than RTMAX
        assert!(min < max);

        // Both should be positive
        assert!(min > 0);
        assert!(max > 0);

        // RTMIN should typically be around 34 on Linux
        // and RTMAX around 64, but we just verify reasonable range
        assert!(min >= 0);
        assert!(max <= 64);
    }

    #[test]
    fn test_sigrtmin_is_rtsig() {
        let min = sigrtmin();
        // sigrtmin should return a valid signal number
        assert!(min >= libc::SIGRTMIN as c_int);
    }

    #[test]
    fn test_sigrtmax_is_rtsig() {
        let max = sigrtmax();
        // sigrtmax should return a valid signal number
        assert!(max <= libc::SIGRTMAX as c_int);
    }
}
