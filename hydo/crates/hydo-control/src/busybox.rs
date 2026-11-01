//! busybox's words for a failed `mount` (docs/HISTORY.md M131).
//!
//! hydo-init.sh mounts the data disk with `MERR=$($BB mount -t ext4 -o "$HYDO_DATA_OPTS" /dev/vda2
//! /data 2>&1) || die_oracle "$1 ($MERR)"`, so the verdict of an unmountable disk holds busybox's
//! own error text. On an agent image the agent's `mount` op does the mount(2) call and refuses with
//! the errno; [`mount_error`] writes what busybox `mount` prints for that errno, so both kinds of
//! image give one verdict (and the explorer one signature) for one failure.
//!
//! What busybox 1.36.1 and 1.37.0 do (`util-linux/mount.c`, `mount_it_now` and `singlemount`):
//! - a failed mount(2) ends in `bb_perror_msg("mounting %s on %s failed", ...)`:
//!   `mount: mounting <src> on <dst> failed: <strerror(errno)>`;
//! - with EACCES or EROFS and no `ro` it first prints `mount: <src> is write-protected, mounting
//!   read-only` and tries again read-only ([`retries_read_only`]); the message of a second failure
//!   follows on the next line, and a read-only mount that works is a mount that worked;
//! - EPERM ends in `mount: permission denied (are you root?)`.
//!
//! strerror is musl's, linked into the static busybox: its table (`src/errno/__strerror.h`) maps
//! every code it does not list to "No error information". The two pinned busyboxes differ there
//! (docs/BUILD-PINS.md): the arm64 one (1.36.1) has musl 1.2.x's table, without EUCLEAN, so a
//! corrupt ext4 (EFSCORRUPTED = EUCLEAN, 117) gives "No error information"; the x86_64 one (1.37.0)
//! has musl's later table with EUCLEAN "Data consistency error" and ENAVAIL "Resource not
//! available". [`Strerror::of_busybox`] tells them apart by the image's own busybox bytes. The
//! table below is musl's master `__strerror.h` with the Linux errno numbers (the same on x86_64
//! and arm64); both busyboxes' tables were read out of the pinned binaries and equal musl's
//! headers entry for entry (M131).

/// Which musl table the image's busybox carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strerror {
    /// musl 1.2.x releases (the pinned arm64 busybox 1.36.1): no EUCLEAN, no ENAVAIL.
    #[default]
    Musl12,
    /// musl's later table (the pinned x86_64 busybox 1.37.0): EUCLEAN and ENAVAIL have words.
    MuslEuclean,
}

const EUCLEAN: i64 = 117;
const ENAVAIL: i64 = 119;
const EPERM: i64 = 1;
const EACCES: i64 = 13;
const EROFS: i64 = 30;

impl Strerror {
    /// The table of a busybox binary: the later one when it has EUCLEAN's words (musl keeps its
    /// messages as one block of NUL-ended strings in the header's order, EUCLEAN's right before
    /// ENAVAIL's).
    pub fn of_busybox(bytes: &[u8]) -> Strerror {
        let needle = b"Data consistency error\0Resource not available\0";
        if bytes.windows(needle.len()).any(|w| w == needle) {
            Strerror::MuslEuclean
        } else {
            Strerror::Musl12
        }
    }

    /// strerror(errno) as this musl writes it.
    pub fn text(self, errno: i64) -> &'static str {
        if self == Strerror::Musl12 && (errno == EUCLEAN || errno == ENAVAIL) {
            return "No error information";
        }
        musl(errno)
    }
}

/// musl's table (master), errno -> words; "No error information" for every other code.
fn musl(errno: i64) -> &'static str {
    match errno {
        84 => "Illegal byte sequence",                    // EILSEQ
        33 => "Domain error",                             // EDOM
        34 => "Result not representable",                 // ERANGE
        25 => "Not a tty",                                // ENOTTY
        13 => "Permission denied",                        // EACCES
        1 => "Operation not permitted",                   // EPERM
        2 => "No such file or directory",                 // ENOENT
        3 => "No such process",                           // ESRCH
        17 => "File exists",                              // EEXIST
        75 => "Value too large for data type",            // EOVERFLOW
        28 => "No space left on device",                  // ENOSPC
        12 => "Out of memory",                            // ENOMEM
        16 => "Resource busy",                            // EBUSY
        4 => "Interrupted system call",                   // EINTR
        11 => "Resource temporarily unavailable",         // EAGAIN
        29 => "Invalid seek",                             // ESPIPE
        18 => "Cross-device link",                        // EXDEV
        30 => "Read-only file system",                    // EROFS
        39 => "Directory not empty",                      // ENOTEMPTY
        104 => "Connection reset by peer",                // ECONNRESET
        110 => "Operation timed out",                     // ETIMEDOUT
        111 => "Connection refused",                      // ECONNREFUSED
        112 => "Host is down",                            // EHOSTDOWN
        113 => "Host is unreachable",                     // EHOSTUNREACH
        98 => "Address in use",                           // EADDRINUSE
        32 => "Broken pipe",                              // EPIPE
        5 => "I/O error",                                 // EIO
        6 => "No such device or address",                 // ENXIO
        15 => "Block device required",                    // ENOTBLK
        19 => "No such device",                           // ENODEV
        20 => "Not a directory",                          // ENOTDIR
        21 => "Is a directory",                           // EISDIR
        26 => "Text file busy",                           // ETXTBSY
        8 => "Exec format error",                         // ENOEXEC
        22 => "Invalid argument",                         // EINVAL
        7 => "Argument list too long",                    // E2BIG
        40 => "Symbolic link loop",                       // ELOOP
        36 => "Filename too long",                        // ENAMETOOLONG
        23 => "Too many open files in system",            // ENFILE
        24 => "No file descriptors available",            // EMFILE
        9 => "Bad file descriptor",                       // EBADF
        10 => "No child process",                         // ECHILD
        14 => "Bad address",                              // EFAULT
        27 => "File too large",                           // EFBIG
        31 => "Too many links",                           // EMLINK
        37 => "No locks available",                       // ENOLCK
        35 => "Resource deadlock would occur",            // EDEADLK
        131 => "State not recoverable",                   // ENOTRECOVERABLE
        130 => "Previous owner died",                     // EOWNERDEAD
        125 => "Operation canceled",                      // ECANCELED
        38 => "Function not implemented",                 // ENOSYS
        42 => "No message of desired type",               // ENOMSG
        43 => "Identifier removed",                       // EIDRM
        60 => "Device not a stream",                      // ENOSTR
        61 => "No data available",                        // ENODATA
        62 => "Device timeout",                           // ETIME
        63 => "Out of streams resources",                 // ENOSR
        67 => "Link has been severed",                    // ENOLINK
        71 => "Protocol error",                           // EPROTO
        74 => "Bad message",                              // EBADMSG
        77 => "File descriptor in bad state",             // EBADFD
        88 => "Not a socket",                             // ENOTSOCK
        89 => "Destination address required",             // EDESTADDRREQ
        90 => "Message too large",                        // EMSGSIZE
        91 => "Protocol wrong type for socket",           // EPROTOTYPE
        92 => "Protocol not available",                   // ENOPROTOOPT
        93 => "Protocol not supported",                   // EPROTONOSUPPORT
        94 => "Socket type not supported",                // ESOCKTNOSUPPORT
        95 => "Not supported",                            // ENOTSUP
        96 => "Protocol family not supported",            // EPFNOSUPPORT
        97 => "Address family not supported by protocol", // EAFNOSUPPORT
        99 => "Address not available",                    // EADDRNOTAVAIL
        100 => "Network is down",                         // ENETDOWN
        101 => "Network unreachable",                     // ENETUNREACH
        102 => "Connection reset by network",             // ENETRESET
        103 => "Connection aborted",                      // ECONNABORTED
        105 => "No buffer space available",               // ENOBUFS
        106 => "Socket is connected",                     // EISCONN
        107 => "Socket not connected",                    // ENOTCONN
        108 => "Cannot send after socket shutdown",       // ESHUTDOWN
        114 => "Operation already in progress",           // EALREADY
        115 => "Operation in progress",                   // EINPROGRESS
        116 => "Stale file handle",                       // ESTALE
        117 => "Data consistency error",                  // EUCLEAN
        119 => "Resource not available",                  // ENAVAIL
        121 => "Remote I/O error",                        // EREMOTEIO
        122 => "Quota exceeded",                          // EDQUOT
        123 => "No medium found",                         // ENOMEDIUM
        124 => "Wrong medium type",                       // EMEDIUMTYPE
        72 => "Multihop attempted",                       // EMULTIHOP
        126 => "Required key not available",              // ENOKEY
        127 => "Key has expired",                         // EKEYEXPIRED
        128 => "Key has been revoked",                    // EKEYREVOKED
        129 => "Key was rejected by service",             // EKEYREJECTED
        _ => "No error information",
    }
}

/// Whether busybox tries the mount again read-only after this errno (`flags` = the mount's own
/// flags have `ro`: then it does not).
pub fn retries_read_only(errno: i64, already_ro: bool) -> bool {
    !already_ro && (errno == EACCES || errno == EROFS)
}

/// The line busybox prints before the read-only attempt.
pub fn write_protected(src: &str) -> String {
    format!("mount: {src} is write-protected, mounting read-only")
}

/// The line busybox `mount` prints for a mount(2) of `src` on `dst` that failed with `errno`.
pub fn mount_error(src: &str, dst: &str, errno: i64, table: Strerror) -> String {
    if errno == EPERM {
        return "mount: permission denied (are you root?)".to_string();
    }
    format!(
        "mount: mounting {src} on {dst} failed: {}",
        table.text(errno)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_of_the_shell_images() {
        // what the shell images printed for a lying disk (C2 on the Mac, M128; x86_64 M94)
        assert_eq!(
            mount_error("/dev/vda2", "/data", 117, Strerror::Musl12),
            "mount: mounting /dev/vda2 on /data failed: No error information"
        );
        assert_eq!(
            mount_error("/dev/vda2", "/data", 117, Strerror::MuslEuclean),
            "mount: mounting /dev/vda2 on /data failed: Data consistency error"
        );
        assert_eq!(
            mount_error("/dev/vda2", "/data", 22, Strerror::Musl12),
            "mount: mounting /dev/vda2 on /data failed: Invalid argument"
        );
        assert_eq!(
            mount_error("/dev/vda2", "/data", 5, Strerror::MuslEuclean),
            "mount: mounting /dev/vda2 on /data failed: I/O error"
        );
        assert_eq!(
            mount_error("/dev/vda2", "/data", 74, Strerror::Musl12),
            "mount: mounting /dev/vda2 on /data failed: Bad message"
        );
        assert_eq!(
            mount_error("/dev/vda2", "/data", 1, Strerror::Musl12),
            "mount: permission denied (are you root?)"
        );
        assert_eq!(Strerror::Musl12.text(119), "No error information");
        assert_eq!(Strerror::MuslEuclean.text(119), "Resource not available");
        assert_eq!(Strerror::MuslEuclean.text(0), "No error information");
        assert_eq!(Strerror::MuslEuclean.text(118), "No error information");
        assert_eq!(Strerror::MuslEuclean.text(4095), "No error information");
        assert!(retries_read_only(30, false) && retries_read_only(13, false));
        assert!(!retries_read_only(30, true) && !retries_read_only(117, false));
    }

    #[test]
    fn the_table_is_told_by_the_busybox_bytes() {
        let older = b"..Key was rejected by service\0/bin/sh\0".to_vec();
        assert_eq!(Strerror::of_busybox(&older), Strerror::Musl12);
        let later =
            b"..Stale file handle\0Data consistency error\0Resource not available\0Remote I/O error\0";
        assert_eq!(Strerror::of_busybox(later), Strerror::MuslEuclean);
        assert_eq!(Strerror::of_busybox(b""), Strerror::Musl12);
    }

    #[test]
    fn every_listed_code_has_its_own_words() {
        // 94 codes in musl's master table besides 0, each with other words
        let mut seen = std::collections::BTreeSet::new();
        let mut n = 0;
        for e in 1..200 {
            let t = Strerror::MuslEuclean.text(e);
            if t != "No error information" {
                assert!(seen.insert(t), "{e}: {t} twice");
                n += 1;
            }
        }
        assert_eq!(n, 94);
    }
}
