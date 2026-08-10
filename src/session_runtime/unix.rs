//! Unix primitives owned by a Session Runtime.

use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::path::PathBuf;

/// A non-blocking advisory lease that remains held for as long as this value lives.
pub(crate) struct LifetimeLease {
    _file: File,
}

impl LifetimeLease {
    /// Attempts to acquire the exclusive lease at `path` without waiting.
    pub(crate) fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        if !file.metadata()?.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("lease path `{}` is not a regular file", path.display()),
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;

        // `flock` locks the open file description, so retaining `file` is the authority for
        // the lease. The pathname is intentionally never removed when the lease is dropped.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(Some(Self { _file: file }));
        }

        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EACCES) | Some(libc::EAGAIN) => Ok(None),
            _ => Err(error),
        }
    }

    /// Returns whether another process currently holds the lease, without creating state.
    pub(crate) fn is_held(path: &Path) -> io::Result<bool> {
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if !file.metadata()?.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("lease path `{}` is not a regular file", path.display()),
            ));
        }

        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            return Ok(false);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EACCES) | Some(libc::EAGAIN) => Ok(true),
            _ => Err(error),
        }
    }
}

/// Binds a listener whose directory and socket are private to the current user.
pub(crate) fn bind_private_listener(
    runtime_dir: &Path,
    socket_path: &Path,
) -> io::Result<UnixListener> {
    if let Some(control_root) = runtime_dir.parent() {
        ensure_private_directory(control_root)?;
    }
    ensure_private_directory(runtime_dir)?;

    match fs::symlink_metadata(socket_path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(socket_path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "refusing to replace non-socket runtime control path `{}`",
                    socket_path.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let listener = UnixListener::bind(socket_path)?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub(crate) fn ensure_private_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "private runtime path `{}` is not a real directory",
                    path.display()
                ),
            ));
        }
        Ok(metadata) if metadata.uid() != unsafe { libc::geteuid() } => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "private runtime path `{}` is not owned by the current user",
                    path.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
        }
        Err(error) => return Err(error),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Returns the effective user ID of the peer connected to `stream`.
#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut uid = 0;
    let mut gid = 0;
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Resolves the executable currently running in the peer process.
///
/// This is deliberately stronger than same-UID authentication for cooperative handoff: the
/// caller must actually be the executable identity it asks the runtime to replace with.
#[cfg(target_os = "macos")]
pub(crate) fn peer_executable_identity(stream: &UnixStream) -> io::Result<PathBuf> {
    let mut pid: libc::pid_t = 0;
    let mut length = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &raw mut length,
        )
    };
    if result != 0 || pid <= 0 {
        return Err(io::Error::last_os_error());
    }

    let mut path = [0_i8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let length = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    if length <= 0 {
        return Err(io::Error::last_os_error());
    }
    let bytes = unsafe { CStr::from_ptr(path.as_ptr()) }.to_bytes().to_vec();
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// Resolves the executable currently running in the peer process.
#[cfg(target_os = "linux")]
pub(crate) fn peer_executable_identity(stream: &UnixStream) -> io::Result<PathBuf> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if result != 0 || credentials.pid <= 0 {
        return Err(io::Error::last_os_error());
    }
    fs::read_link(format!("/proc/{}/exe", credentials.pid))
}

/// Returns an unsupported-platform error where peer executable identity is unavailable.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn peer_executable_identity(_stream: &UnixStream) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix peer executable lookup is unsupported on this platform",
    ))
}

/// Returns the effective user ID of the peer connected to `stream`.
#[cfg(target_os = "linux")]
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if result == 0 {
        Ok(credentials.uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Returns an unsupported-platform error where Unix peer credentials are unavailable.
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
pub(crate) fn peer_uid(_stream: &UnixStream) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix peer credential lookup is unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    static NEXT_TEMP_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let suffix = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = PathBuf::from("/tmp").join(format!("tby-{}-{suffix}", std::process::id()));
            fs::create_dir(&path).expect("create temporary directory");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).expect("remove temporary directory");
        }
    }

    #[test]
    fn lifetime_lease_is_exclusive_until_released() {
        let directory = TempDir::new();
        let path = directory.path.join("runtime.lock");

        let lease = LifetimeLease::try_acquire(&path)
            .expect("acquire first lease")
            .expect("first lease is available");
        assert!(
            LifetimeLease::try_acquire(&path)
                .expect("attempt second lease")
                .is_none()
        );

        drop(lease);
        assert!(
            LifetimeLease::try_acquire(&path)
                .expect("acquire released lease")
                .is_some()
        );
    }

    #[test]
    fn lifetime_lease_reports_ownership_without_creating_a_missing_file() {
        let directory = TempDir::new();
        let path = directory.path.join("runtime.lock");

        assert!(!LifetimeLease::is_held(&path).expect("missing lease is not held"));
        assert!(!path.exists());

        let lease = LifetimeLease::try_acquire(&path)
            .expect("acquire lease")
            .expect("lease available");
        assert!(LifetimeLease::is_held(&path).expect("lease is held"));

        drop(lease);
        assert!(!LifetimeLease::is_held(&path).expect("released lease is not held"));
    }

    #[test]
    fn lease_process_helper() {
        let Some(path) = std::env::var_os("TABBY_RUNTIME_LEASE_HELPER_PATH") else {
            return;
        };
        let path = PathBuf::from(path);
        let _lease = LifetimeLease::try_acquire(&path)
            .expect("child acquires lease")
            .expect("lease is available to child");
        fs::write(path.with_extension("ready"), b"ready").expect("child signals readiness");
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }

    #[test]
    fn process_exit_releases_the_lifetime_lease_for_a_new_owner() {
        let directory = TempDir::new();
        let path = directory.path.join("runtime.lock");
        let ready_path = path.with_extension("ready");
        let test_binary = std::env::current_exe().expect("test binary");
        let mut child = Command::new(test_binary)
            .args([
                "--exact",
                "session_runtime::unix::tests::lease_process_helper",
                "--nocapture",
            ])
            .env("TABBY_RUNTIME_LEASE_HELPER_PATH", &path)
            .spawn()
            .expect("spawn lease holder");

        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready_path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ready_path.exists(),
            "child did not acquire the lease in time"
        );
        assert!(LifetimeLease::is_held(&path).expect("inspect child lease"));
        assert!(
            LifetimeLease::try_acquire(&path)
                .expect("attempt concurrent lease")
                .is_none()
        );

        child.kill().expect("terminate lease holder");
        child.wait().expect("reap lease holder");
        assert!(
            LifetimeLease::try_acquire(&path)
                .expect("acquire after child exit")
                .is_some()
        );
    }

    #[test]
    fn private_listener_restricts_directory_and_socket_permissions() {
        let directory = TempDir::new();
        let runtime_dir = directory.path.join("runtime");
        let socket_path = runtime_dir.join("control.sock");

        let _listener = bind_private_listener(&runtime_dir, &socket_path).expect("bind listener");

        assert_eq!(
            fs::metadata(runtime_dir.parent().expect("control root"))
                .expect("control root directory")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&runtime_dir)
                .expect("runtime directory")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&socket_path)
                .expect("control socket")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
