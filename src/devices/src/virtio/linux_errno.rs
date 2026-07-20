#![cfg_attr(target_os = "windows", allow(dead_code))]

const LINUX_EPERM: i32 = 1;
const LINUX_ENOENT: i32 = 2;
const LINUX_ESRCH: i32 = 3;
const LINUX_EINTR: i32 = 4;
const LINUX_EIO: i32 = 5;
const LINUX_ENXIO: i32 = 6;
const LINUX_ENOEXEC: i32 = 8;
const LINUX_EBADF: i32 = 9;
const LINUX_ECHILD: i32 = 10;
const LINUX_EAGAIN: i32 = 11;
const LINUX_ENOMEM: i32 = 12;
const LINUX_EACCES: i32 = 13;
const LINUX_EFAULT: i32 = 14;
const LINUX_ENOTBLK: i32 = 15;
const LINUX_EBUSY: i32 = 16;
const LINUX_EEXIST: i32 = 17;
const LINUX_EXDEV: i32 = 18;
const LINUX_ENODEV: i32 = 19;
const LINUX_ENOTDIR: i32 = 20;
const LINUX_EISDIR: i32 = 21;
const LINUX_EINVAL: i32 = 22;
const LINUX_ENFILE: i32 = 23;
const LINUX_EMFILE: i32 = 24;
const LINUX_ENOTTY: i32 = 25;
const LINUX_ETXTBSY: i32 = 26;
const LINUX_EFBIG: i32 = 27;
const LINUX_ENOSPC: i32 = 28;
const LINUX_ESPIPE: i32 = 29;
const LINUX_EROFS: i32 = 30;
const LINUX_EMLINK: i32 = 31;
const LINUX_EPIPE: i32 = 32;
const LINUX_EDOM: i32 = 33;
const LINUX_EDEADLK: i32 = 35;
const LINUX_ENAMETOOLONG: i32 = 36;
const LINUX_ENOLCK: i32 = 37;
const LINUX_ENOSYS: i32 = 38;
const LINUX_ENOTEMPTY: i32 = 39;
const LINUX_ELOOP: i32 = 40;
const LINUX_ENOMSG: i32 = 42;
const LINUX_EIDRM: i32 = 43;
const LINUX_ENOSTR: i32 = 60;
const LINUX_ENODATA: i32 = 61;
const LINUX_ETIME: i32 = 62;
const LINUX_ENOSR: i32 = 63;
const LINUX_EREMOTE: i32 = 66;
const LINUX_ENOLINK: i32 = 67;
const LINUX_EPROTO: i32 = 71;
const LINUX_EMULTIHOP: i32 = 72;
const LINUX_EBADMSG: i32 = 74;
const LINUX_EOVERFLOW: i32 = 75;
const LINUX_EILSEQ: i32 = 84;
const LINUX_EUSERS: i32 = 87;
const LINUX_ENOTSOCK: i32 = 88;
const LINUX_EDESTADDRREQ: i32 = 89;
const LINUX_EMSGSIZE: i32 = 90;
const LINUX_EPROTOTYPE: i32 = 91;
const LINUX_ENOPROTOOPT: i32 = 92;
const LINUX_EPROTONOSUPPORT: i32 = 93;
const LINUX_ESOCKTNOSUPPORT: i32 = 94;
const LINUX_EOPNOTSUPP: i32 = 95;
const LINUX_EPFNOSUPPORT: i32 = 96;
const LINUX_EAFNOSUPPORT: i32 = 97;
const LINUX_EADDRINUSE: i32 = 98;
const LINUX_EADDRNOTAVAIL: i32 = 99;
const LINUX_ENETDOWN: i32 = 100;
const LINUX_ENETUNREACH: i32 = 101;
const LINUX_ENETRESET: i32 = 102;
const LINUX_ECONNABORTED: i32 = 103;
const LINUX_ECONNRESET: i32 = 104;
const LINUX_ENOBUFS: i32 = 105;
const LINUX_EISCONN: i32 = 106;
const LINUX_ENOTCONN: i32 = 107;
const LINUX_ESHUTDOWN: i32 = 108;
const LINUX_ETOOMANYREFS: i32 = 109;
const LINUX_ETIMEDOUT: i32 = 110;
const LINUX_ECONNREFUSED: i32 = 111;
const LINUX_EHOSTDOWN: i32 = 112;
const LINUX_EHOSTUNREACH: i32 = 113;
const LINUX_EALREADY: i32 = 114;
const LINUX_EINPROGRESS: i32 = 115;
const LINUX_ESTALE: i32 = 116;
const LINUX_EDQUOT: i32 = 122;
const LINUX_ECANCELED: i32 = 125;
const LINUX_EOWNERDEAD: i32 = 130;
const LINUX_ENOTRECOVERABLE: i32 = 131;

// Errors to be directly used.
pub const LINUX_ERANGE: i32 = 34;

pub fn linux_error(error: std::io::Error) -> std::io::Error {
    std::io::Error::from_raw_os_error(linux_errno_raw(error.raw_os_error().unwrap_or(libc::EIO)))
}

#[cfg(target_os = "windows")]
pub fn linux_errno_raw(errno: i32) -> i32 {
    match errno {
        libc::EPERM => LINUX_EPERM,
        libc::ENOENT => LINUX_ENOENT,
        libc::EINTR => LINUX_EINTR,
        libc::EIO => LINUX_EIO,
        libc::ENXIO => LINUX_ENXIO,
        libc::ENOEXEC => LINUX_ENOEXEC,
        libc::EBADF => LINUX_EBADF,
        libc::ENOMEM => LINUX_ENOMEM,
        libc::EACCES => LINUX_EACCES,
        libc::EFAULT => LINUX_EFAULT,
        libc::EBUSY => LINUX_EBUSY,
        libc::EEXIST => LINUX_EEXIST,
        libc::ENODEV => LINUX_ENODEV,
        libc::ENOTDIR => LINUX_ENOTDIR,
        libc::EISDIR => LINUX_EISDIR,
        libc::EINVAL => LINUX_EINVAL,
        libc::ENFILE => LINUX_ENFILE,
        libc::EMFILE => LINUX_EMFILE,
        libc::ENOTTY => LINUX_ENOTTY,
        libc::EFBIG => LINUX_EFBIG,
        libc::ENOSPC => LINUX_ENOSPC,
        libc::EROFS => LINUX_EROFS,
        libc::EPIPE => LINUX_EPIPE,
        libc::EDOM => LINUX_EDOM,
        libc::EAGAIN => LINUX_EAGAIN,
        libc::EINPROGRESS => LINUX_EINPROGRESS,
        libc::EALREADY => LINUX_EALREADY,
        libc::ENOTSOCK => LINUX_ENOTSOCK,
        libc::EDESTADDRREQ => LINUX_EDESTADDRREQ,
        libc::EMSGSIZE => LINUX_EMSGSIZE,
        libc::EPROTOTYPE => LINUX_EPROTOTYPE,
        libc::ENOPROTOOPT => LINUX_ENOPROTOOPT,
        libc::EPROTONOSUPPORT => LINUX_EPROTONOSUPPORT,
        libc::EAFNOSUPPORT => LINUX_EAFNOSUPPORT,
        libc::EADDRINUSE => LINUX_EADDRINUSE,
        libc::EADDRNOTAVAIL => LINUX_EADDRNOTAVAIL,
        libc::ENETDOWN => LINUX_ENETDOWN,
        libc::ENETUNREACH => LINUX_ENETUNREACH,
        libc::ENETRESET => LINUX_ENETRESET,
        libc::ECONNABORTED => LINUX_ECONNABORTED,
        libc::ECONNRESET => LINUX_ECONNRESET,
        libc::ENOBUFS => LINUX_ENOBUFS,
        libc::EISCONN => LINUX_EISCONN,
        libc::ENOTCONN => LINUX_ENOTCONN,
        libc::ETIMEDOUT => LINUX_ETIMEDOUT,
        libc::ECONNREFUSED => LINUX_ECONNREFUSED,
        libc::ELOOP => LINUX_ELOOP,
        libc::ENAMETOOLONG => LINUX_ENAMETOOLONG,
        libc::EHOSTUNREACH => LINUX_EHOSTUNREACH,
        libc::ENOTEMPTY => LINUX_ENOTEMPTY,
        libc::ENOLCK => LINUX_ENOLCK,
        libc::ENOSYS => LINUX_ENOSYS,
        libc::EOVERFLOW => LINUX_EOVERFLOW,
        libc::ECANCELED => LINUX_ECANCELED,
        _ => LINUX_EIO,
    }
}

#[cfg(not(target_os = "windows"))]
pub fn linux_errno_raw(errno: i32) -> i32 {
    match errno {
        libc::EPERM => LINUX_EPERM,
        libc::ENOENT => LINUX_ENOENT,
        libc::ESRCH => LINUX_ESRCH,
        libc::EINTR => LINUX_EINTR,
        libc::EIO => LINUX_EIO,
        libc::ENXIO => LINUX_ENXIO,
        libc::ENOEXEC => LINUX_ENOEXEC,
        libc::EBADF => LINUX_EBADF,
        libc::ECHILD => LINUX_ECHILD,
        libc::EDEADLK => LINUX_EDEADLK,
        libc::ENOMEM => LINUX_ENOMEM,
        libc::EACCES => LINUX_EACCES,
        libc::EFAULT => LINUX_EFAULT,
        libc::ENOTBLK => LINUX_ENOTBLK,
        libc::EBUSY => LINUX_EBUSY,
        libc::EEXIST => LINUX_EEXIST,
        libc::EXDEV => LINUX_EXDEV,
        libc::ENODEV => LINUX_ENODEV,
        libc::ENOTDIR => LINUX_ENOTDIR,
        libc::EISDIR => LINUX_EISDIR,
        libc::EINVAL => LINUX_EINVAL,
        libc::ENFILE => LINUX_ENFILE,
        libc::EMFILE => LINUX_EMFILE,
        libc::ENOTTY => LINUX_ENOTTY,
        libc::ETXTBSY => LINUX_ETXTBSY,
        libc::EFBIG => LINUX_EFBIG,
        libc::ENOSPC => LINUX_ENOSPC,
        libc::ESPIPE => LINUX_ESPIPE,
        libc::EROFS => LINUX_EROFS,
        libc::EMLINK => LINUX_EMLINK,
        libc::EPIPE => LINUX_EPIPE,
        libc::EDOM => LINUX_EDOM,
        libc::EAGAIN => LINUX_EAGAIN,
        libc::EINPROGRESS => LINUX_EINPROGRESS,
        libc::EALREADY => LINUX_EALREADY,
        libc::ENOTSOCK => LINUX_ENOTSOCK,
        libc::EDESTADDRREQ => LINUX_EDESTADDRREQ,
        libc::EMSGSIZE => LINUX_EMSGSIZE,
        libc::EPROTOTYPE => LINUX_EPROTOTYPE,
        libc::ENOPROTOOPT => LINUX_ENOPROTOOPT,
        libc::EPROTONOSUPPORT => LINUX_EPROTONOSUPPORT,
        libc::ESOCKTNOSUPPORT => LINUX_ESOCKTNOSUPPORT,
        libc::EPFNOSUPPORT => LINUX_EPFNOSUPPORT,
        libc::EAFNOSUPPORT => LINUX_EAFNOSUPPORT,
        libc::EADDRINUSE => LINUX_EADDRINUSE,
        libc::EADDRNOTAVAIL => LINUX_EADDRNOTAVAIL,
        libc::ENETDOWN => LINUX_ENETDOWN,
        libc::ENETUNREACH => LINUX_ENETUNREACH,
        libc::ENETRESET => LINUX_ENETRESET,
        libc::ECONNABORTED => LINUX_ECONNABORTED,
        libc::ECONNRESET => LINUX_ECONNRESET,
        libc::ENOBUFS => LINUX_ENOBUFS,
        libc::EISCONN => LINUX_EISCONN,
        libc::ENOTCONN => LINUX_ENOTCONN,
        libc::ESHUTDOWN => LINUX_ESHUTDOWN,
        libc::ETOOMANYREFS => LINUX_ETOOMANYREFS,
        libc::ETIMEDOUT => LINUX_ETIMEDOUT,
        libc::ECONNREFUSED => LINUX_ECONNREFUSED,
        libc::ELOOP => LINUX_ELOOP,
        libc::ENAMETOOLONG => LINUX_ENAMETOOLONG,
        libc::EHOSTDOWN => LINUX_EHOSTDOWN,
        libc::EHOSTUNREACH => LINUX_EHOSTUNREACH,
        libc::ENOTEMPTY => LINUX_ENOTEMPTY,
        libc::EUSERS => LINUX_EUSERS,
        libc::EDQUOT => LINUX_EDQUOT,
        libc::ESTALE => LINUX_ESTALE,
        libc::EREMOTE => LINUX_EREMOTE,
        libc::ENOLCK => LINUX_ENOLCK,
        libc::ENOSYS => LINUX_ENOSYS,
        libc::EOVERFLOW => LINUX_EOVERFLOW,
        libc::ECANCELED => LINUX_ECANCELED,
        libc::EIDRM => LINUX_EIDRM,
        libc::ENOMSG => LINUX_ENOMSG,
        libc::EILSEQ => LINUX_EILSEQ,
        #[cfg(target_os = "macos")]
        libc::ENOATTR => LINUX_ENODATA,
        libc::EBADMSG => LINUX_EBADMSG,
        libc::EMULTIHOP => LINUX_EMULTIHOP,
        libc::ENODATA => LINUX_ENODATA,
        libc::ENOLINK => LINUX_ENOLINK,
        libc::ENOSR => LINUX_ENOSR,
        libc::ENOSTR => LINUX_ENOSTR,
        libc::EPROTO => LINUX_EPROTO,
        libc::ETIME => LINUX_ETIME,
        libc::EOPNOTSUPP => LINUX_EOPNOTSUPP,
        libc::ENOTRECOVERABLE => LINUX_ENOTRECOVERABLE,
        libc::EOWNERDEAD => LINUX_EOWNERDEAD,
        _ => LINUX_EIO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn test_linux_errno_raw_known_errors() {
        assert_eq!(linux_errno_raw(libc::EPERM), LINUX_EPERM);
        assert_eq!(linux_errno_raw(libc::ENOENT), LINUX_ENOENT);
        assert_eq!(linux_errno_raw(libc::ESRCH), LINUX_ESRCH);
        assert_eq!(linux_errno_raw(libc::EINTR), LINUX_EINTR);
        assert_eq!(linux_errno_raw(libc::EIO), LINUX_EIO);
        assert_eq!(linux_errno_raw(libc::ENXIO), LINUX_ENXIO);
        assert_eq!(linux_errno_raw(libc::ENOEXEC), LINUX_ENOEXEC);
        assert_eq!(linux_errno_raw(libc::EBADF), LINUX_EBADF);
        assert_eq!(linux_errno_raw(libc::ECHILD), LINUX_ECHILD);
        assert_eq!(linux_errno_raw(libc::EDEADLK), LINUX_EDEADLK);
        assert_eq!(linux_errno_raw(libc::ENOMEM), LINUX_ENOMEM);
        assert_eq!(linux_errno_raw(libc::EACCES), LINUX_EACCES);
        assert_eq!(linux_errno_raw(libc::EFAULT), LINUX_EFAULT);
        assert_eq!(linux_errno_raw(libc::ENOTBLK), LINUX_ENOTBLK);
        assert_eq!(linux_errno_raw(libc::EBUSY), LINUX_EBUSY);
        assert_eq!(linux_errno_raw(libc::EEXIST), LINUX_EEXIST);
        assert_eq!(linux_errno_raw(libc::EXDEV), LINUX_EXDEV);
        assert_eq!(linux_errno_raw(libc::ENODEV), LINUX_ENODEV);
        assert_eq!(linux_errno_raw(libc::ENOTDIR), LINUX_ENOTDIR);
        assert_eq!(linux_errno_raw(libc::EISDIR), LINUX_EISDIR);
        assert_eq!(linux_errno_raw(libc::EINVAL), LINUX_EINVAL);
        assert_eq!(linux_errno_raw(libc::ENFILE), LINUX_ENFILE);
        assert_eq!(linux_errno_raw(libc::EMFILE), LINUX_EMFILE);
        assert_eq!(linux_errno_raw(libc::ENOTTY), LINUX_ENOTTY);
        assert_eq!(linux_errno_raw(libc::ETXTBSY), LINUX_ETXTBSY);
        assert_eq!(linux_errno_raw(libc::EFBIG), LINUX_EFBIG);
        assert_eq!(linux_errno_raw(libc::ENOSPC), LINUX_ENOSPC);
        assert_eq!(linux_errno_raw(libc::ESPIPE), LINUX_ESPIPE);
        assert_eq!(linux_errno_raw(libc::EROFS), LINUX_EROFS);
        assert_eq!(linux_errno_raw(libc::EMLINK), LINUX_EMLINK);
        assert_eq!(linux_errno_raw(libc::EPIPE), LINUX_EPIPE);
        assert_eq!(linux_errno_raw(libc::EDOM), LINUX_EDOM);
        assert_eq!(linux_errno_raw(libc::EAGAIN), LINUX_EAGAIN);
        assert_eq!(linux_errno_raw(libc::EINPROGRESS), LINUX_EINPROGRESS);
        assert_eq!(linux_errno_raw(libc::EALREADY), LINUX_EALREADY);
        assert_eq!(linux_errno_raw(libc::ENOTSOCK), LINUX_ENOTSOCK);
        assert_eq!(linux_errno_raw(libc::EDESTADDRREQ), LINUX_EDESTADDRREQ);
        assert_eq!(linux_errno_raw(libc::EMSGSIZE), LINUX_EMSGSIZE);
        assert_eq!(linux_errno_raw(libc::EPROTOTYPE), LINUX_EPROTOTYPE);
        assert_eq!(linux_errno_raw(libc::ENOPROTOOPT), LINUX_ENOPROTOOPT);
        assert_eq!(
            linux_errno_raw(libc::EPROTONOSUPPORT),
            LINUX_EPROTONOSUPPORT
        );
        assert_eq!(
            linux_errno_raw(libc::ESOCKTNOSUPPORT),
            LINUX_ESOCKTNOSUPPORT
        );
        assert_eq!(linux_errno_raw(libc::EPFNOSUPPORT), LINUX_EPFNOSUPPORT);
        assert_eq!(linux_errno_raw(libc::EAFNOSUPPORT), LINUX_EAFNOSUPPORT);
        assert_eq!(linux_errno_raw(libc::EADDRINUSE), LINUX_EADDRINUSE);
        assert_eq!(linux_errno_raw(libc::EADDRNOTAVAIL), LINUX_EADDRNOTAVAIL);
        assert_eq!(linux_errno_raw(libc::ENETDOWN), LINUX_ENETDOWN);
        assert_eq!(linux_errno_raw(libc::ENETUNREACH), LINUX_ENETUNREACH);
        assert_eq!(linux_errno_raw(libc::ENETRESET), LINUX_ENETRESET);
        assert_eq!(linux_errno_raw(libc::ECONNABORTED), LINUX_ECONNABORTED);
        assert_eq!(linux_errno_raw(libc::ECONNRESET), LINUX_ECONNRESET);
        assert_eq!(linux_errno_raw(libc::ENOBUFS), LINUX_ENOBUFS);
        assert_eq!(linux_errno_raw(libc::EISCONN), LINUX_EISCONN);
        assert_eq!(linux_errno_raw(libc::ENOTCONN), LINUX_ENOTCONN);
        assert_eq!(linux_errno_raw(libc::ESHUTDOWN), LINUX_ESHUTDOWN);
        assert_eq!(linux_errno_raw(libc::ETOOMANYREFS), LINUX_ETOOMANYREFS);
        assert_eq!(linux_errno_raw(libc::ETIMEDOUT), LINUX_ETIMEDOUT);
        assert_eq!(linux_errno_raw(libc::ECONNREFUSED), LINUX_ECONNREFUSED);
        assert_eq!(linux_errno_raw(libc::ELOOP), LINUX_ELOOP);
        assert_eq!(linux_errno_raw(libc::ENAMETOOLONG), LINUX_ENAMETOOLONG);
        assert_eq!(linux_errno_raw(libc::EHOSTDOWN), LINUX_EHOSTDOWN);
        assert_eq!(linux_errno_raw(libc::EHOSTUNREACH), LINUX_EHOSTUNREACH);
        assert_eq!(linux_errno_raw(libc::ENOTEMPTY), LINUX_ENOTEMPTY);
        assert_eq!(linux_errno_raw(libc::EUSERS), LINUX_EUSERS);
        assert_eq!(linux_errno_raw(libc::EDQUOT), LINUX_EDQUOT);
        assert_eq!(linux_errno_raw(libc::ESTALE), LINUX_ESTALE);
        assert_eq!(linux_errno_raw(libc::EREMOTE), LINUX_EREMOTE);
        assert_eq!(linux_errno_raw(libc::ENOLCK), LINUX_ENOLCK);
        assert_eq!(linux_errno_raw(libc::ENOSYS), LINUX_ENOSYS);
        assert_eq!(linux_errno_raw(libc::EOVERFLOW), LINUX_EOVERFLOW);
        assert_eq!(linux_errno_raw(libc::ECANCELED), LINUX_ECANCELED);
        assert_eq!(linux_errno_raw(libc::EIDRM), LINUX_EIDRM);
        assert_eq!(linux_errno_raw(libc::ENOMSG), LINUX_ENOMSG);
        assert_eq!(linux_errno_raw(libc::EILSEQ), LINUX_EILSEQ);
        assert_eq!(linux_errno_raw(libc::EBADMSG), LINUX_EBADMSG);
        assert_eq!(linux_errno_raw(libc::EMULTIHOP), LINUX_EMULTIHOP);
        assert_eq!(linux_errno_raw(libc::ENODATA), LINUX_ENODATA);
        assert_eq!(linux_errno_raw(libc::ENOLINK), LINUX_ENOLINK);
        assert_eq!(linux_errno_raw(libc::ENOSR), LINUX_ENOSR);
        assert_eq!(linux_errno_raw(libc::ENOSTR), LINUX_ENOSTR);
        assert_eq!(linux_errno_raw(libc::EPROTO), LINUX_EPROTO);
        assert_eq!(linux_errno_raw(libc::ETIME), LINUX_ETIME);
        assert_eq!(linux_errno_raw(libc::EOPNOTSUPP), LINUX_EOPNOTSUPP);
        assert_eq!(
            linux_errno_raw(libc::ENOTRECOVERABLE),
            LINUX_ENOTRECOVERABLE
        );
        assert_eq!(linux_errno_raw(libc::EOWNERDEAD), LINUX_EOWNERDEAD);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_linux_errno_raw_unknown_error() {
        // Unknown errno should map to LINUX_EIO
        assert_eq!(linux_errno_raw(99999), LINUX_EIO);
        assert_eq!(linux_errno_raw(0), LINUX_EIO);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_linux_errno_raw_known_errors_macos() {
        // Test common errors
        assert_eq!(linux_errno_raw(libc::EPERM), LINUX_EPERM);
        assert_eq!(linux_errno_raw(libc::ENOENT), LINUX_ENOENT);
        assert_eq!(linux_errno_raw(libc::EINTR), LINUX_EINTR);
        assert_eq!(linux_errno_raw(libc::EIO), LINUX_EIO);
        assert_eq!(linux_errno_raw(libc::ENOMEM), LINUX_ENOMEM);
        assert_eq!(linux_errno_raw(libc::EACCES), LINUX_EACCES);
        assert_eq!(linux_errno_raw(libc::EINVAL), LINUX_EINVAL);
        assert_eq!(linux_errno_raw(libc::ENOATTR), LINUX_ENODATA);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_linux_errno_raw_unknown_error_macos() {
        assert_eq!(linux_errno_raw(99999), LINUX_EIO);
    }

    #[test]
    fn test_linux_error_eperm() {
        let io_err = std::io::Error::from_raw_os_error(libc::EPERM);
        let result = linux_error(io_err);
        assert_eq!(result.raw_os_error(), Some(LINUX_EPERM));
    }

    #[test]
    fn test_linux_error_enoent() {
        let io_err = std::io::Error::from_raw_os_error(libc::ENOENT);
        let result = linux_error(io_err);
        assert_eq!(result.raw_os_error(), Some(LINUX_ENOENT));
    }

    #[test]
    fn test_linux_error_eio() {
        let io_err = std::io::Error::from_raw_os_error(libc::EIO);
        let result = linux_error(io_err);
        assert_eq!(result.raw_os_error(), Some(LINUX_EIO));
    }

    #[test]
    fn test_linux_error_eagain() {
        let io_err = std::io::Error::from_raw_os_error(libc::EAGAIN);
        let result = linux_error(io_err);
        assert_eq!(result.raw_os_error(), Some(LINUX_EAGAIN));
    }

    #[test]
    fn test_linux_error_enoexec() {
        let io_err = std::io::Error::from_raw_os_error(libc::ENOEXEC);
        let result = linux_error(io_err);
        assert_eq!(result.raw_os_error(), Some(LINUX_ENOEXEC));
    }

    #[test]
    fn test_linux_error_enomem() {
        let io_err = std::io::Error::from_raw_os_error(libc::ENOMEM);
        let result = linux_error(io_err);
        assert_eq!(result.raw_os_error(), Some(LINUX_ENOMEM));
    }

    #[test]
    fn test_linux_error_einval() {
        let io_err = std::io::Error::from_raw_os_error(libc::EINVAL);
        let result = linux_error(io_err);
        assert_eq!(result.raw_os_error(), Some(LINUX_EINVAL));
    }

    #[test]
    fn test_linux_error_unknown() {
        // When raw_os_error returns None, it should default to LINUX_EIO
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "custom error");
        let result = linux_error(io_err);
        assert_eq!(result.raw_os_error(), Some(LINUX_EIO));
    }

    #[test]
    fn test_linux_constants() {
        // Verify Linux error constants have expected values
        assert_eq!(LINUX_EPERM, 1);
        assert_eq!(LINUX_ENOENT, 2);
        assert_eq!(LINUX_ESRCH, 3);
        assert_eq!(LINUX_EINTR, 4);
        assert_eq!(LINUX_EIO, 5);
        assert_eq!(LINUX_ENXIO, 6);
        assert_eq!(LINUX_ENOEXEC, 8);
        assert_eq!(LINUX_EBADF, 9);
        assert_eq!(LINUX_ECHILD, 10);
        assert_eq!(LINUX_EAGAIN, 11);
        assert_eq!(LINUX_ENOMEM, 12);
        assert_eq!(LINUX_EACCES, 13);
        assert_eq!(LINUX_EFAULT, 14);
        assert_eq!(LINUX_ENOTBLK, 15);
        assert_eq!(LINUX_EBUSY, 16);
        assert_eq!(LINUX_EEXIST, 17);
        assert_eq!(LINUX_EXDEV, 18);
        assert_eq!(LINUX_ENODEV, 19);
        assert_eq!(LINUX_ENOTDIR, 20);
        assert_eq!(LINUX_EISDIR, 21);
        assert_eq!(LINUX_EINVAL, 22);
        assert_eq!(LINUX_ENFILE, 23);
        assert_eq!(LINUX_EMFILE, 24);
        assert_eq!(LINUX_ENOTTY, 25);
        assert_eq!(LINUX_ETXTBSY, 26);
        assert_eq!(LINUX_EFBIG, 27);
        assert_eq!(LINUX_ENOSPC, 28);
        assert_eq!(LINUX_ESPIPE, 29);
        assert_eq!(LINUX_EROFS, 30);
        assert_eq!(LINUX_EMLINK, 31);
        assert_eq!(LINUX_EPIPE, 32);
        assert_eq!(LINUX_EDOM, 33);
        assert_eq!(LINUX_ERANGE, 34);
        assert_eq!(LINUX_EDEADLK, 35);
        assert_eq!(LINUX_ENAMETOOLONG, 36);
        assert_eq!(LINUX_ENOLCK, 37);
        assert_eq!(LINUX_ENOSYS, 38);
        assert_eq!(LINUX_ENOTEMPTY, 39);
        assert_eq!(LINUX_ELOOP, 40);
        assert_eq!(LINUX_ENOMSG, 42);
        assert_eq!(LINUX_EIDRM, 43);
        assert_eq!(LINUX_ENOSTR, 60);
        assert_eq!(LINUX_ENODATA, 61);
        assert_eq!(LINUX_ETIME, 62);
        assert_eq!(LINUX_ENOSR, 63);
        assert_eq!(LINUX_EREMOTE, 66);
        assert_eq!(LINUX_ENOLINK, 67);
        assert_eq!(LINUX_EPROTO, 71);
        assert_eq!(LINUX_EMULTIHOP, 72);
        assert_eq!(LINUX_EBADMSG, 74);
        assert_eq!(LINUX_EOVERFLOW, 75);
        assert_eq!(LINUX_EILSEQ, 84);
        assert_eq!(LINUX_EUSERS, 87);
        assert_eq!(LINUX_ENOTSOCK, 88);
        assert_eq!(LINUX_EDESTADDRREQ, 89);
        assert_eq!(LINUX_EMSGSIZE, 90);
        assert_eq!(LINUX_EPROTOTYPE, 91);
        assert_eq!(LINUX_ENOPROTOOPT, 92);
        assert_eq!(LINUX_EPROTONOSUPPORT, 93);
        assert_eq!(LINUX_ESOCKTNOSUPPORT, 94);
        assert_eq!(LINUX_EOPNOTSUPP, 95);
        assert_eq!(LINUX_EPFNOSUPPORT, 96);
        assert_eq!(LINUX_EAFNOSUPPORT, 97);
        assert_eq!(LINUX_EADDRINUSE, 98);
        assert_eq!(LINUX_EADDRNOTAVAIL, 99);
        assert_eq!(LINUX_ENETDOWN, 100);
        assert_eq!(LINUX_ENETUNREACH, 101);
        assert_eq!(LINUX_ENETRESET, 102);
        assert_eq!(LINUX_ECONNABORTED, 103);
        assert_eq!(LINUX_ECONNRESET, 104);
        assert_eq!(LINUX_ENOBUFS, 105);
        assert_eq!(LINUX_EISCONN, 106);
        assert_eq!(LINUX_ENOTCONN, 107);
        assert_eq!(LINUX_ESHUTDOWN, 108);
        assert_eq!(LINUX_ETOOMANYREFS, 109);
        assert_eq!(LINUX_ETIMEDOUT, 110);
        assert_eq!(LINUX_ECONNREFUSED, 111);
        assert_eq!(LINUX_EHOSTDOWN, 112);
        assert_eq!(LINUX_EHOSTUNREACH, 113);
        assert_eq!(LINUX_EALREADY, 114);
        assert_eq!(LINUX_EINPROGRESS, 115);
        assert_eq!(LINUX_ESTALE, 116);
        assert_eq!(LINUX_EDQUOT, 122);
        assert_eq!(LINUX_ECANCELED, 125);
        assert_eq!(LINUX_EOWNERDEAD, 130);
        assert_eq!(LINUX_ENOTRECOVERABLE, 131);
    }
}
