//! Live session directories for repository grouping. The public session
//! list retains its launch-directory contract; Git grouping uses the
//! kernel-owned child's current directory when the OS can report it.

use std::path::PathBuf;

use crate::cli::json::SessionSummary;
use crate::session::paths::LatchHome;

pub(crate) fn refresh_directories(home: &LatchHome, sessions: &mut [SessionSummary]) {
    let Ok(live) = crate::engine::list(home) else {
        return;
    };
    for session in sessions
        .iter_mut()
        .filter(|session| session.state == "running")
    {
        if let Some(info) = live.iter().find(|info| info.id == session.id) {
            if let Some(cwd) = process_directory(info.pane_pid) {
                session.cwd = cwd;
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn process_directory(pid: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    if pid <= 1 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>();
    // SAFETY: proc_pidinfo writes at most `size` bytes to this correctly
    // aligned structure. Read it only when the full structure was returned.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if read != size as i32 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let bytes = info
        .pvi_cdir
        .vip_path
        .into_iter()
        .flatten()
        .take_while(|byte| *byte != 0)
        .map(|byte| byte as u8)
        .collect::<Vec<_>>();
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    path.is_absolute().then_some(path)
}

#[cfg(target_os = "linux")]
fn process_directory(pid: i32) -> Option<PathBuf> {
    (pid > 1)
        .then(|| std::fs::read_link(format!("/proc/{pid}/cwd")).ok())
        .flatten()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_directory(_pid: i32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_childs_current_directory_can_differ_from_its_launch_directory() {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let moved = temp.path().join("moved");
        std::fs::create_dir(&moved).unwrap();
        let mut child = std::process::Command::new("/bin/sh")
            .current_dir(temp.path())
            .args([
                "-c",
                "cd -- \"$1\"; printf ready; exec /bin/sleep 30",
                "verify",
                moved.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Read;
        let mut ready = [0; 5];
        child.stdout.take().unwrap().read_exact(&mut ready).unwrap();
        let found = process_directory(child.id() as i32);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(
            found.and_then(|path| path.canonicalize().ok()),
            Some(moved.canonicalize().unwrap())
        );
        assert!(process_directory(-1).is_none());
    }
}
