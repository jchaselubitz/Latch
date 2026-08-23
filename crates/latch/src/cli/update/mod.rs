//! `latch update` — replace this install with the newest published release.
//!
//! Latch is installed as one signed payload — `latch`, `latch-remote`, and
//! `latch-tmux` — so it has to be updated by replacing that payload. That is
//! the whole feature, and the risk is entirely in the details: the archive
//! must be the one the release published, the swap must not leave a
//! half-written binary where a working one was, and a copy that somebody else
//! owns — a Homebrew cellar, the helper inside `Latch.app` — must be refused
//! rather than quietly diverged from its package.
//!
//! Three checks stand between the network and the installed binaries:
//!
//! 1. the archive's SHA-256 must match the release's own `checksums.txt`;
//! 2. when the running binary carries a Developer ID signature, the downloaded
//!    ones must carry a valid signature from the same team;
//! 3. each replacement is staged beside the target and moved into place with
//!    `rename`, so every installed path is either the old binary or the new one.
//!
//! Networking is `curl`, and unpacking is `ditto`/`tar`. Both are part of
//! macOS, and the alternative is a TLS stack and an archive reader linked into
//! a binary whose start-up cost every terminal window pays (decision D2).

pub mod release;
pub mod sha256;

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};

use crate::cli::json::UpdateReport;
use crate::engine::{BUNDLED_REMOTE_NAME, BUNDLED_TMUX_NAME};
use release::{host_target, Release, Version, DEFAULT_RELEASE_REPO};

/// Extra files that ship beside `latch` in a release archive.
const PAYLOAD_SIBLINGS: &[&str] = &[BUNDLED_TMUX_NAME, BUNDLED_REMOTE_NAME];

/// What an update run was asked to do.
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    /// Version this binary reports as its own.
    pub current_version: String,
    /// Report what is available without installing it.
    pub check_only: bool,
    /// Install the published release even when it is not newer.
    ///
    /// The repair path: a truncated or hand-edited binary reports whatever
    /// version it was built as, so "already current" is not the same as
    /// "already correct".
    pub force: bool,
    /// Binary to replace. Defaults to the running executable.
    pub executable: Option<PathBuf>,
    /// Release target triple to install. Defaults to this machine's.
    pub target: Option<String>,
}

impl UpdateOptions {
    /// Options for the running binary on this machine.
    pub fn new(current_version: impl Into<String>) -> Self {
        Self {
            current_version: current_version.into(),
            check_only: false,
            force: false,
            executable: None,
            target: None,
        }
    }
}

/// What `update` did.
///
/// `Current` and `Available` are the two outcomes of a check; a run that
/// installs reports `Installed` with the version now on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStatus {
    /// The installed version is the published one.
    Current,
    /// A newer release exists and was not installed.
    Available,
    /// A newer release was installed.
    Installed,
}

impl UpdateStatus {
    /// The wire spelling used by `--json`.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Available => "available",
            Self::Installed => "installed",
        }
    }
}

/// Where release metadata and archives come from.
///
/// A trait so the install path can be exercised without a network: the tests
/// serve a release document and archives from a directory, and everything
/// downstream of the fetch — digest verification, unpacking, the atomic
/// swap — runs for real.
pub trait ReleaseSource {
    /// The newest published release, as the GitHub releases API renders it.
    fn latest_release(&self) -> Result<String>;
    /// Downloads `url` to `destination`.
    fn download(&self, url: &str, destination: &Path) -> Result<()>;
    /// Fetches a small text file, such as `checksums.txt`.
    fn fetch_text(&self, url: &str) -> Result<String>;
}

/// The published GitHub releases of one repository.
#[derive(Debug, Clone)]
pub struct GitHubReleases {
    repo: String,
}

impl Default for GitHubReleases {
    fn default() -> Self {
        Self {
            repo: DEFAULT_RELEASE_REPO.to_owned(),
        }
    }
}

impl ReleaseSource for GitHubReleases {
    fn latest_release(&self) -> Result<String> {
        let url = format!("https://api.github.com/repos/{}/releases/latest", self.repo);
        let body = curl(&["--header", "Accept: application/vnd.github+json", &url])
            .with_context(|| format!("could not reach the Latch release feed at {url}"))?;
        String::from_utf8(body).context("the release feed was not valid UTF-8")
    }

    fn download(&self, url: &str, destination: &Path) -> Result<()> {
        let path = destination
            .to_str()
            .ok_or_else(|| anyhow!("download path {} is not UTF-8", destination.display()))?;
        curl(&["--output", path, url])
            .with_context(|| format!("could not download {url}"))
            .map(|_| ())
    }

    fn fetch_text(&self, url: &str) -> Result<String> {
        let body = curl(&[url]).with_context(|| format!("could not download {url}"))?;
        String::from_utf8(body).with_context(|| format!("{url} was not valid UTF-8"))
    }
}

/// Runs `curl` with the flags every request needs and returns its stdout.
///
/// `--proto =https` is the load-bearing one: without it a redirect can move
/// the download to plain HTTP, and `--location` is required because release
/// asset URLs redirect to object storage.
fn curl(arguments: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-time",
            "300",
            "--user-agent",
            concat!("latch/", env!("CARGO_PKG_VERSION")),
        ])
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .context("could not run `curl`, which `latch update` uses to fetch releases")?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        bail!("{}", diagnostic.trim());
    }
    Ok(output.stdout)
}

/// Whether `latch update` could replace this install in place.
///
/// Reported by `latch capabilities` so a client — the desktop app — can tell
/// the difference between an update it can offer and one that has to be done
/// through a package manager or by updating the app.
pub fn can_self_update() -> bool {
    if host_target().is_err() {
        return false;
    }
    match std::env::current_exe() {
        Ok(path) => {
            let resolved = fs::canonicalize(&path).unwrap_or(path);
            refuse_managed_location(&resolved).is_ok()
        }
        Err(_) => false,
    }
}

/// Checks for, and unless asked not to installs, the newest published release.
pub fn update(options: UpdateOptions) -> Result<UpdateReport> {
    update_from(options, &GitHubReleases::default())
}

/// [`update`], against a caller-supplied release source.
pub fn update_from(options: UpdateOptions, source: &dyn ReleaseSource) -> Result<UpdateReport> {
    let current = Version::parse(&options.current_version)?;
    let release = Release::parse(&source.latest_release()?)?;

    let newer = release.version > current;
    let payload_incomplete = !payload_is_complete(options.executable.as_deref());
    if !newer && !options.force && !payload_incomplete {
        return Ok(UpdateReport {
            status: UpdateStatus::Current.as_wire().to_owned(),
            current_version: current.to_string(),
            latest_version: Some(release.version.to_string()),
            release_url: Some(release.url),
            installed_path: None,
        });
    }

    if options.check_only {
        return Ok(UpdateReport {
            status: UpdateStatus::Available.as_wire().to_owned(),
            current_version: current.to_string(),
            latest_version: Some(release.version.to_string()),
            release_url: Some(release.url),
            installed_path: None,
        });
    }

    let triple = match &options.target {
        Some(triple) => triple.clone(),
        None => host_target()?.to_owned(),
    };
    let target = match options.executable {
        Some(path) => path,
        None => std::env::current_exe().context("could not locate the running `latch` binary")?,
    };
    // A symlinked install (Homebrew's `bin/latch`, a `~/.local/bin` shim)
    // must have the real file replaced, not the link: renaming over the link
    // would break the package that owns it.
    let target = fs::canonicalize(&target)
        .with_context(|| format!("could not resolve {}", target.display()))?;
    refuse_managed_location(&target)?;

    let installed = install(source, &release, &target, &triple)?;

    Ok(UpdateReport {
        status: UpdateStatus::Installed.as_wire().to_owned(),
        current_version: current.to_string(),
        latest_version: Some(release.version.to_string()),
        release_url: Some(release.url),
        installed_path: Some(installed),
    })
}

fn payload_is_complete(executable: Option<&Path>) -> bool {
    let executable = executable
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_exe().ok());
    let Some(executable) = executable else {
        return false;
    };
    let executable = fs::canonicalize(&executable).unwrap_or(executable);
    executable.parent().is_some_and(|parent| {
        PAYLOAD_SIBLINGS
            .iter()
            .all(|name| parent.join(name).is_file())
            && kernel_is_patched(&parent.join(BUNDLED_TMUX_NAME))
    })
}

/// Whether the installed `latch-tmux` is the Latch-patched kernel.
///
/// A present file with the right name and upstream version is not enough:
/// stock tmux 3.7b satisfies both and then fails every attach at the moment a
/// user wants one. `-R` is the raw-attach flag the patched kernel accepts
/// during client identification and upstream tmux rejects as an unknown
/// option, so probing it here turns "your kernel is not the Latch one" into a
/// payload repair rather than a runtime error.
///
/// A staged file that is not yet executable — mid-install, or a test fixture
/// written with `fs::write` — cannot be probed, so treat only a definite
/// rejection as incomplete and let the ordinary version comparison decide the
/// rest.
fn kernel_is_patched(tmux: &Path) -> bool {
    match Command::new(tmux)
        .args(["-R", "-V"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => true,
    }
}

/// Refuses to replace a binary that something else is responsible for.
///
/// The helper inside `Latch.app` is covered by the app's code signature:
/// replacing it makes the app fail Gatekeeper on next launch. A Homebrew
/// cellar copy is owned by a formula, and silently diverging from it turns the
/// next `brew upgrade` into a surprise.
fn refuse_managed_location(target: &Path) -> Result<()> {
    let display = target.display();
    if target
        .components()
        .any(|component| component.as_os_str().to_string_lossy().ends_with(".app"))
    {
        bail!(
            "{display} is the `latch` bundled inside a Latch.app; replacing it would break the \
             app's signature. Update the app from Latch → Check for Updates instead."
        );
    }
    if target.starts_with("/opt/homebrew/Cellar") || target.starts_with("/usr/local/Cellar") {
        bail!("{display} is managed by Homebrew; run `brew upgrade latch` instead.");
    }
    if target.starts_with("/nix/store") {
        bail!("{display} is in the Nix store, which is read-only; update it through Nix instead.");
    }
    Ok(())
}

/// Downloads, verifies, and swaps in the release build for this machine.
fn install(
    source: &dyn ReleaseSource,
    release: &Release,
    target: &Path,
    triple: &str,
) -> Result<PathBuf> {
    let archive = release.cli_archive(triple).ok_or_else(|| {
        anyhow!(
            "release {} has no {triple} archive; download one from {} by hand",
            release.tag,
            release.url
        )
    })?;

    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", target.display()))?;
    // Staging inside the destination directory keeps the final step a
    // same-filesystem `rename`, which is the only way the swap is atomic, and
    // it fails here — before anything is downloaded — when the directory is
    // not writable.
    let staging = Staging::create(parent, target)?;

    let archive_path = staging.path().join(&archive.name);
    source.download(&archive.url, &archive_path)?;
    verify_digest(source, release, &archive.name, &archive_path)?;

    let unpacked = staging.path().join("unpacked");
    fs::create_dir_all(&unpacked)
        .with_context(|| format!("could not create {}", unpacked.display()))?;
    unpack(&archive_path, &unpacked)?;

    let replacement = unpacked.join("latch");
    let siblings: Vec<(&str, PathBuf, PathBuf)> = PAYLOAD_SIBLINGS
        .iter()
        .map(|name| (*name, unpacked.join(name), parent.join(name)))
        .collect();
    if !replacement.is_file() || siblings.iter().any(|(_, source, _)| !source.is_file()) {
        bail!(
            "{} did not contain the complete `latch`, `{BUNDLED_TMUX_NAME}`, and `{BUNDLED_REMOTE_NAME}` payload",
            archive.name
        );
    }
    verify_signature(target, &replacement)?;
    for (_, source, _) in &siblings {
        verify_signature(target, source)?;
    }

    // Match the mode of what is being replaced, so an install that was
    // deliberately group-readable-only stays that way; fall back to the mode
    // `install -m 755` gives a fresh copy.
    let mode = fs::metadata(target)
        .map(|meta| meta.permissions().mode() & 0o7777)
        .unwrap_or(0o755);
    fs::set_permissions(&replacement, fs::Permissions::from_mode(mode))
        .with_context(|| format!("could not set permissions on {}", replacement.display()))?;
    for (_, source, _) in &siblings {
        fs::set_permissions(source, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("could not set permissions on {}", source.display()))?;
    }

    let mut backups = Vec::new();
    for (name, source, destination) in &siblings {
        let backup = staging.path().join(format!("previous-{name}"));
        if destination.is_file() {
            fs::copy(destination, &backup)
                .with_context(|| format!("could not stage {}", destination.display()))?;
        }
        if let Err(error) = fs::rename(source, destination) {
            restore_siblings(&backups);
            return Err(error)
                .with_context(|| format!("could not replace {}", destination.display()));
        }
        backups.push((backup, destination.clone()));
    }
    if let Err(error) = fs::rename(&replacement, target) {
        restore_siblings(&backups);
        return Err(error).with_context(|| {
            format!(
                "could not replace {}. The bundled payload was rolled back; re-run with write access to {}",
                target.display(),
                parent.display()
            )
        });
    }
    Ok(target.to_path_buf())
}

/// Restores payload siblings after a failed swap. Best-effort: a restore
/// error must not hide the install failure that triggered it.
fn restore_siblings(backups: &[(PathBuf, PathBuf)]) {
    for (backup, destination) in backups.iter().rev() {
        if backup.is_file() {
            let _ = fs::rename(backup, destination);
        } else {
            let _ = fs::remove_file(destination);
        }
    }
}

/// Hashes the downloaded archive and compares it with the release's manifest.
fn verify_digest(
    source: &dyn ReleaseSource,
    release: &Release,
    asset_name: &str,
    archive: &Path,
) -> Result<()> {
    let manifest = release.checksums().ok_or_else(|| {
        anyhow!(
            "release {} publishes no checksums.txt, so the download cannot be verified",
            release.tag
        )
    })?;
    let document = source.fetch_text(&manifest.url)?;
    let expected = release::expected_digest(&document, asset_name).ok_or_else(|| {
        anyhow!(
            "checksums.txt for {} does not list {asset_name}",
            release.tag
        )
    })?;
    let actual = sha256::hex_of_file(archive)
        .with_context(|| format!("could not read {}", archive.display()))?;
    if actual != expected {
        bail!(
            "{asset_name} does not match the checksum {} published for it \
             (got {actual}); nothing was installed",
            expected
        );
    }
    Ok(())
}

/// Unpacks a release archive, whichever of the two published formats it is.
///
/// `ditto` rather than `unzip` for ZIPs: it is what created the archive on the
/// release machine, and it preserves the extended attributes a signed binary
/// carries.
fn unpack(archive: &Path, into: &Path) -> Result<()> {
    let name = archive.file_name().unwrap_or_default().to_string_lossy();
    let (program, arguments): (&str, Vec<&std::ffi::OsStr>) = if name.ends_with(".zip") {
        (
            "ditto",
            vec![
                "-x".as_ref(),
                "-k".as_ref(),
                archive.as_os_str(),
                into.as_os_str(),
            ],
        )
    } else {
        (
            "tar",
            vec![
                "-xzf".as_ref(),
                archive.as_os_str(),
                "-C".as_ref(),
                into.as_os_str(),
            ],
        )
    };
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not run `{program}` to unpack {name}"))?;
    if !output.status.success() {
        bail!(
            "could not unpack {name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Requires the replacement to be signed by whoever signed the running binary.
///
/// Conditional on the current binary being signed at all, so a `cargo build`
/// copy can still update itself, but a Developer ID install can never be
/// replaced by something signed by somebody else — the check the checksum
/// cannot make, because a manifest and the archive it describes come from the
/// same place.
fn verify_signature(current: &Path, replacement: &Path) -> Result<()> {
    let expected_team = match signing_team(current) {
        Some(team) => team,
        None => return Ok(()),
    };
    match signing_team(replacement) {
        Some(team) if team == expected_team => Ok(()),
        Some(team) => bail!(
            "the downloaded binary is signed by team {team}, but this install is signed by \
             {expected_team}; nothing was installed"
        ),
        None => bail!(
            "the downloaded binary is not validly signed, but this install is (team \
             {expected_team}); nothing was installed"
        ),
    }
}

/// The Team ID of a validly signed binary, or `None` when it is unsigned,
/// ad-hoc signed, or when `codesign` is unavailable.
fn signing_team(path: &Path) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let verified = Command::new("codesign")
        .args(["--verify", "--strict"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !verified.status.success() {
        return None;
    }
    let described = Command::new("codesign")
        .args(["--display", "--verbose=4"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    // `codesign --display` writes its description to stderr.
    let text = String::from_utf8_lossy(&described.stderr);
    text.lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set")
        .map(str::to_owned)
}

/// A scratch directory beside the binary being replaced, removed on drop.
struct Staging {
    path: PathBuf,
}

impl Staging {
    fn create(parent: &Path, target: &Path) -> Result<Self> {
        let name = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "latch".to_owned());
        let path = parent.join(format!(".{name}-update-{}", std::process::id()));
        if let Err(error) = fs::remove_dir_all(&path) {
            if error.kind() != ErrorKind::NotFound {
                return Err(error).with_context(|| format!("could not clear {}", path.display()));
            }
        }
        fs::create_dir_all(&path).with_context(|| {
            format!(
                "could not write to {} to stage the update. Re-run with write access to it, \
                 or reinstall Latch by hand.",
                parent.display()
            )
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A release served from local files, so the download, verification,
    /// unpacking, and swap all run for real without a network.
    struct FakeSource {
        document: String,
        files: HashMap<String, Vec<u8>>,
        fetched: Mutex<Vec<String>>,
    }

    impl ReleaseSource for FakeSource {
        fn latest_release(&self) -> Result<String> {
            Ok(self.document.clone())
        }

        fn download(&self, url: &str, destination: &Path) -> Result<()> {
            self.fetched.lock().unwrap().push(url.to_owned());
            let body = self
                .files
                .get(url)
                .ok_or_else(|| anyhow!("no such asset: {url}"))?;
            fs::write(destination, body)?;
            Ok(())
        }

        fn fetch_text(&self, url: &str) -> Result<String> {
            let body = self
                .files
                .get(url)
                .ok_or_else(|| anyhow!("no such asset: {url}"))?;
            Ok(String::from_utf8(body.clone())?)
        }
    }

    /// The machine the fixture releases are built for. Pinned rather than
    /// probed so the suite runs the same way on the Linux CI leg, where there
    /// is no published build for the host.
    const FIXTURE_TARGET: &str = "aarch64-apple-darwin";

    /// Options that install a fixture release over `target`.
    fn options_for(target: &Path, current_version: &str) -> UpdateOptions {
        UpdateOptions {
            executable: Some(target.to_path_buf()),
            target: Some(FIXTURE_TARGET.to_owned()),
            ..UpdateOptions::new(current_version)
        }
    }

    /// Builds a release whose archive contains `body` as the `latch` binary.
    fn release_with(version: &str, body: &[u8], corrupt_digest: bool) -> FakeSource {
        let dir = tempfile::tempdir().expect("temp dir");
        let staged = dir.path().join("latch");
        fs::write(&staged, body).expect("stage binary");
        let staged_tmux = dir.path().join(BUNDLED_TMUX_NAME);
        fs::write(&staged_tmux, b"tmux 3.7b").expect("stage tmux");
        let staged_remote = dir.path().join(BUNDLED_REMOTE_NAME);
        fs::write(&staged_remote, b"the remote helper").expect("stage remote");
        let archive_name = format!("latch-{version}-{FIXTURE_TARGET}.tar.gz");
        let archive = dir.path().join(&archive_name);
        let status = Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .args(["-C"])
            .arg(dir.path())
            .args(["latch", BUNDLED_TMUX_NAME, BUNDLED_REMOTE_NAME])
            .status()
            .expect("tar");
        assert!(status.success(), "tar must produce the fixture archive");
        let bytes = fs::read(&archive).expect("read archive");
        let digest = if corrupt_digest {
            "0".repeat(64)
        } else {
            sha256::hex(&bytes)
        };

        let archive_url = format!("https://example.invalid/{archive_name}");
        let checksums_url = "https://example.invalid/checksums.txt".to_owned();
        let document = format!(
            r#"{{"tag_name":"v{version}","html_url":"https://example.invalid/tag","assets":[
                {{"name":"{archive_name}","browser_download_url":"{archive_url}"}},
                {{"name":"checksums.txt","browser_download_url":"{checksums_url}"}}]}}"#
        );
        let mut files = HashMap::new();
        files.insert(archive_url, bytes);
        files.insert(
            checksums_url,
            format!("{digest}  dist/{archive_name}\n").into_bytes(),
        );
        FakeSource {
            document,
            files,
            fetched: Mutex::new(Vec::new()),
        }
    }

    /// Writes a stand-in `latch-tmux`.
    ///
    /// Payload completeness probes the kernel's raw-attach capability, so the
    /// fixture has to answer `-R -V` the way a real kernel does: patched ones
    /// succeed, an upstream build rejects the unknown option.
    fn write_kernel(path: &Path, patched: bool) {
        let body = if patched {
            "#!/bin/sh\ncase \"$1\" in -R) exit 0 ;; esac\nexit 1\n"
        } else {
            "#!/bin/sh\nexit 1\n"
        };
        fs::write(path, body).expect("write kernel");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod kernel");
    }

    fn installed_binary(directory: &Path) -> PathBuf {
        let path = directory.join("latch");
        fs::write(&path, b"the old binary").expect("write old binary");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        write_kernel(&directory.join(BUNDLED_TMUX_NAME), true);
        let remote = directory.join(BUNDLED_REMOTE_NAME);
        fs::write(&remote, b"the old remote").expect("write old remote");
        fs::set_permissions(&remote, fs::Permissions::from_mode(0o755)).expect("chmod remote");
        path
    }

    #[test]
    fn an_older_installed_version_is_replaced_by_the_published_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = installed_binary(dir.path());
        let source = release_with("0.2608101202.0", b"the new binary", false);

        let report = update_from(options_for(&target, "0.2608100841.0"), &source).expect("update");

        assert_eq!(report.status, "installed");
        assert_eq!(report.latest_version.as_deref(), Some("0.2608101202.0"));
        assert_eq!(fs::read(&target).expect("read"), b"the new binary");
        assert_eq!(
            fs::read(dir.path().join(BUNDLED_TMUX_NAME)).expect("read tmux"),
            b"tmux 3.7b"
        );
        assert_eq!(
            fs::read(dir.path().join(BUNDLED_REMOTE_NAME)).expect("read remote"),
            b"the remote helper"
        );
        assert_eq!(
            fs::metadata(&target).expect("stat").permissions().mode() & 0o777,
            0o755,
            "the replacement must stay executable"
        );
    }

    #[test]
    fn an_archive_that_does_not_match_the_published_checksum_is_not_installed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = installed_binary(dir.path());
        let source = release_with("0.2608101202.0", b"tampered", true);

        let error = update_from(options_for(&target, "0.2608100841.0"), &source)
            .expect_err("a checksum mismatch must fail the update");

        assert!(
            format!("{error:#}").contains("does not match the checksum"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&target).expect("read"),
            b"the old binary",
            "a failed update must leave the working binary in place"
        );
    }

    #[test]
    fn a_current_install_is_left_alone_and_reported_as_current() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = installed_binary(dir.path());
        let source = release_with("0.2608101202.0", b"the new binary", false);

        let report = update_from(options_for(&target, "0.2608101202.0"), &source).expect("update");

        assert_eq!(report.status, "current");
        assert!(report.installed_path.is_none());
        assert_eq!(fs::read(&target).expect("read"), b"the old binary");
        assert!(
            source.fetched.lock().unwrap().is_empty(),
            "a check that finds nothing newer must not download anything"
        );
    }

    #[test]
    fn a_current_cli_with_a_missing_tmux_repairs_the_complete_payload() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = installed_binary(dir.path());
        fs::remove_file(dir.path().join(BUNDLED_TMUX_NAME)).expect("remove tmux");
        let source = release_with("0.2608101202.0", b"the repaired binary", false);

        let report = update_from(options_for(&target, "0.2608101202.0"), &source).expect("repair");

        assert_eq!(report.status, "installed");
        assert_eq!(fs::read(&target).unwrap(), b"the repaired binary");
        assert_eq!(
            fs::read(dir.path().join(BUNDLED_TMUX_NAME)).unwrap(),
            b"tmux 3.7b"
        );
        assert_eq!(
            fs::read(dir.path().join(BUNDLED_REMOTE_NAME)).unwrap(),
            b"the remote helper"
        );
    }

    #[test]
    fn a_current_cli_beside_an_unpatched_kernel_repairs_the_complete_payload() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = installed_binary(dir.path());
        write_kernel(&dir.path().join(BUNDLED_TMUX_NAME), false);
        let source = release_with("0.2608101202.0", b"the repaired binary", false);

        let report = update_from(options_for(&target, "0.2608101202.0"), &source).expect("repair");

        assert_eq!(
            report.status, "installed",
            "an upstream tmux beside `latch` is an incomplete payload, not a current one: \
             every attach would fail at the moment a user wanted one"
        );
        assert_eq!(
            fs::read(dir.path().join(BUNDLED_TMUX_NAME)).unwrap(),
            b"tmux 3.7b"
        );
    }

    #[test]
    fn a_current_cli_with_a_missing_remote_helper_repairs_the_complete_payload() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = installed_binary(dir.path());
        fs::remove_file(dir.path().join(BUNDLED_REMOTE_NAME)).expect("remove remote");
        let source = release_with("0.2608101202.0", b"the repaired binary", false);

        let report = update_from(options_for(&target, "0.2608101202.0"), &source).expect("repair");

        assert_eq!(report.status, "installed");
        assert_eq!(fs::read(&target).unwrap(), b"the repaired binary");
        assert_eq!(
            fs::read(dir.path().join(BUNDLED_REMOTE_NAME)).unwrap(),
            b"the remote helper"
        );
    }

    #[test]
    fn force_reinstalls_the_same_version() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = installed_binary(dir.path());
        let source = release_with("0.2608101202.0", b"the new binary", false);

        let report = update_from(
            UpdateOptions {
                force: true,
                ..options_for(&target, "0.2608101202.0")
            },
            &source,
        )
        .expect("update");

        assert_eq!(report.status, "installed");
        assert_eq!(fs::read(&target).expect("read"), b"the new binary");
    }

    #[test]
    fn check_only_reports_the_newer_release_without_touching_the_binary() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = installed_binary(dir.path());
        let source = release_with("0.2608101202.0", b"the new binary", false);

        let report = update_from(
            UpdateOptions {
                check_only: true,
                ..options_for(&target, "0.2608100841.0")
            },
            &source,
        )
        .expect("update");

        assert_eq!(report.status, "available");
        assert!(report.installed_path.is_none());
        assert_eq!(fs::read(&target).expect("read"), b"the old binary");
    }

    #[test]
    fn a_failed_update_leaves_no_staging_directory_behind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = installed_binary(dir.path());
        let source = release_with("0.2608101202.0", b"tampered", true);

        let _ = update_from(options_for(&target, "0.2608100841.0"), &source);

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                name != "latch" && name != BUNDLED_TMUX_NAME && name != BUNDLED_REMOTE_NAME
            })
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[test]
    fn the_helper_inside_an_app_bundle_is_refused_by_name() {
        let error =
            refuse_managed_location(Path::new("/Applications/Latch.app/Contents/MacOS/latch"))
                .expect_err("a bundled helper must be refused");
        assert!(format!("{error:#}").contains("Latch.app"), "{error:#}");

        let error = refuse_managed_location(Path::new("/opt/homebrew/Cellar/latch/1.0/bin/latch"))
            .expect_err("a Homebrew copy must be refused");
        assert!(format!("{error:#}").contains("brew upgrade"), "{error:#}");

        refuse_managed_location(Path::new("/usr/local/bin/latch"))
            .expect("an ordinary install is updatable");
    }
}
