use super::*;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;

fn authorized() -> (tempfile::TempDir, LatchHome, String, SharedRootGrant) {
    let temp = tempfile::tempdir().unwrap();
    let home = LatchHome::new(temp.path().join("state"));
    let root = temp.path().join("project");
    fs::create_dir(&root).unwrap();
    crate::cli::remote_access::set_enabled(&home, true).unwrap();
    let device = crate::cli::remote_access::authorize_enrollment(
        &home,
        &format!("enr_{}", "a".repeat(32)),
        &"1".repeat(64),
        "Files test",
        crate::cli::remote_access::DevicePermission::Control,
        &format!("dev_{}", "b".repeat(32)),
    )
    .unwrap();
    let grant = crate::cli::remote_access::share_root(&home, &device.device_id, &root).unwrap();
    (temp, home, device.device_id, grant)
}

#[test]
fn reads_text_with_a_scalar_boundary_and_explicit_binary_and_truncated_outcomes() {
    let (_temp, home, device, grant) = authorized();
    let read = |path| preview(&home, &device, &grant.root_id, path).unwrap();
    fs::write(grant.path.join("text"), "ordinary\nUTF-8 café\n").unwrap();
    fs::write(grant.path.join("binary"), b"data\0ignored").unwrap();
    let prefix = "a".repeat(MAX_PREVIEW_BYTES - 1);
    fs::write(grant.path.join("large"), format!("{prefix}é end")).unwrap();
    fs::write(grant.path.join("exact"), "a".repeat(MAX_PREVIEW_BYTES)).unwrap();
    assert_eq!(
        read("text"),
        SharedFilePreview::Text {
            text: "ordinary\nUTF-8 café\n".into()
        }
    );
    assert_eq!(read("binary"), SharedFilePreview::Binary);
    assert_eq!(read("large"), SharedFilePreview::Truncated { text: prefix });
    assert!(matches!(read("exact"), SharedFilePreview::Text { .. }));
}

#[test]
fn rejects_links_sensitive_paths_and_non_regular_entries() {
    let (temp, home, device, grant) = authorized();
    assert!(root_identity(Path::new("/etc")).is_err());
    assert!(root_identity(Path::new("/private/etc")).is_err());
    let sensitive = grant.path.join(".ssh");
    fs::create_dir(&sensitive).unwrap();
    fs::create_dir(sensitive.join("keys")).unwrap();
    assert!(root_identity(&sensitive.join("keys")).is_err());

    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("file"), "outside").unwrap();
    symlink(&outside, grant.path.join("linked-directory")).unwrap();
    symlink(outside.join("file"), grant.path.join("linked-file")).unwrap();
    fs::hard_link(outside.join("file"), grant.path.join("hard-link")).unwrap();
    fs::write(grant.path.join(".env"), "secret").unwrap();
    fs::write(grant.path.join("TOKEN.PEM"), "secret").unwrap();
    let fifo = CString::new(grant.path.join("pipe").as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is a valid, NUL-terminated fixture path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

    let read = |path| preview(&home, &device, &grant.root_id, path);
    for name in [
        "linked-directory/file",
        "linked-file",
        "hard-link",
        ".env",
        "TOKEN.PEM",
        "pipe",
    ] {
        assert!(matches!(read(name), Err(Error::EntryUnavailable)), "{name}");
    }
    for name in [
        "../outside/file",
        "/etc/passwd",
        "nested/../file",
        "nested//file",
    ] {
        assert!(matches!(read(name), Err(Error::InvalidPath)), "{name}");
    }
    let page = Cursors::default()
        .page(&home, &device, &grant.root_id, String::new(), None)
        .unwrap();
    assert!(page.entries.is_empty());
}

#[test]
fn discards_open_objects_after_link_and_rename_changes() {
    let (temp, _, _, grant) = authorized();
    let dir = grant.path.join("nested");
    fs::create_dir(&dir).unwrap();
    let path = dir.join("file");
    fs::write(&path, "first").unwrap();
    let parts = ["nested", "file"];
    let opened = open_relative(&grant, &parts, false).unwrap();
    let snapshot = opened.metadata().unwrap();
    let moved = temp.path().join("moved");
    fs::rename(&dir, &moved).unwrap();
    symlink(&moved, &dir).unwrap();
    assert!(matches!(
        validate_open(&grant, &parts, &opened, &snapshot, false),
        Err(Error::Changed)
    ));
    assert!(open_relative(&grant, &parts, false).is_err());
    fs::remove_file(&dir).unwrap();
    fs::create_dir(&dir).unwrap();
    fs::write(&path, "replacement").unwrap();
    assert!(matches!(
        validate_open(&grant, &parts, &opened, &snapshot, false),
        Err(Error::Changed)
    ));
    fs::rename(&grant.path, temp.path().join("old-root")).unwrap();
    fs::create_dir(&grant.path).unwrap();
    assert!(matches!(
        open_relative(&grant, &[], true),
        Err(Error::RootUnavailable)
    ));
}

#[test]
fn cursor_is_bound_to_the_directory_snapshot_and_gateway() {
    let (_temp, home, device, grant) = authorized();
    for index in 0..PAGE_SIZE + 1 {
        fs::write(grant.path.join(format!("entry-{index:02}")), []).unwrap();
    }
    fs::create_dir(grant.path.join("subdir")).unwrap();
    let cursors = Cursors::default();
    let first = cursors
        .page(&home, &device, &grant.root_id, String::new(), None)
        .unwrap();
    assert_eq!(first.entries.len(), PAGE_SIZE);
    let token = first.next_cursor.as_deref();
    let second = cursors
        .page(&home, &device, &grant.root_id, String::new(), token)
        .unwrap();
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["entry-64", "subdir"]
    );
    assert!(second.next_cursor.is_none());
    assert!(matches!(
        cursors.page(&home, &device, &grant.root_id, "subdir".into(), token),
        Err(Error::StaleCursor)
    ));
    assert!(matches!(
        Cursors::default().page(&home, &device, &grant.root_id, String::new(), token),
        Err(Error::StaleCursor)
    ));
    fs::write(grant.path.join("new"), []).unwrap();
    assert!(matches!(
        cursors.page(&home, &device, &grant.root_id, String::new(), token),
        Err(Error::StaleCursor)
    ));
}

#[test]
fn revocation_and_pairing_revision_invalidate_in_flight_results() {
    let (_temp, home, device, grant) = authorized();
    assert!(validate_grant(&home, &device, &grant).is_ok());
    crate::cli::remote_access::revoke_shared_root(&home, &grant.root_id).unwrap();
    assert!(matches!(
        validate_grant(&home, &device, &grant),
        Err(Error::RootUnavailable)
    ));
    let replacement = crate::cli::remote_access::share_root(&home, &device, &grant.path).unwrap();
    assert_ne!(replacement.root_id, grant.root_id);

    use crate::cli::remote_access::{grant as change_grant, DevicePermission};
    change_grant(&home, &device, DevicePermission::Observe).unwrap();
    assert!(matches!(
        validate_grant(&home, &device, &replacement),
        Err(Error::Denied)
    ));
    change_grant(&home, &device, DevicePermission::Control).unwrap();
    assert!(matches!(
        validate_grant(&home, &device, &replacement),
        Err(Error::RootUnavailable)
    ));
}
