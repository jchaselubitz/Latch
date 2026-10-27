//! Mac-local provider configuration. No environment credential fallback.
use std::io::{BufRead, Read, Write};
use std::path::Path;

use anyhow::{bail, Context};
use zeroize::Zeroizing;

use crate::session::paths::LatchHome;

/// Default, independently configurable without a mobile release.
pub const DEFAULT_MODEL: &str = "gemini-3.5-flash-lite";
#[cfg(target_os = "macos")]
const SERVICE: &str = "dev.cooperativ.latch.commit-message.gemini";
#[cfg(target_os = "macos")]
const ACCOUNT: &str = "api-key";

/// Disable Keychain UI for the entire dedicated gateway process. Access that
/// requires a GUI prompt must report unavailable, never hang an HTTP request.
pub(crate) fn headless_keychain() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let guard =
            security_framework::os::macos::keychain::SecKeychain::disable_user_interaction()
                .map_err(|_| anyhow::anyhow!("cannot disable Keychain interaction"))?;
        // This process remains noninteractive until exit (no restoration).
        std::mem::forget(guard);
    }
    Ok(())
}

pub(crate) enum ConfigError {
    Missing,
    Unavailable,
}

pub(crate) struct ProviderConfig {
    pub key: Zeroizing<String>,
    pub model: String,
}

pub(crate) fn load(home: &LatchHome) -> Result<ProviderConfig, ConfigError> {
    let model = model(home).map_err(|_| ConfigError::Unavailable)?;
    let bytes = load_key()?;
    let key =
        Zeroizing::new(String::from_utf8(bytes.to_vec()).map_err(|_| ConfigError::Unavailable)?);
    if !valid_key(&key) {
        return Err(ConfigError::Unavailable);
    }
    Ok(ProviderConfig { key, model })
}

#[cfg(all(target_os = "macos", not(test)))]
fn load_key() -> Result<Zeroizing<Vec<u8>>, ConfigError> {
    security_framework::passwords::get_generic_password(SERVICE, ACCOUNT)
        .map(Zeroizing::new)
        .map_err(|error| {
            if error.code() == -25300 {
                ConfigError::Missing
            } else {
                ConfigError::Unavailable
            }
        })
}

#[cfg(any(not(target_os = "macos"), test))]
fn load_key() -> Result<Zeroizing<Vec<u8>>, ConfigError> {
    // Tests never use a person's Keychain; no plaintext fallback on other OSes.
    Err(ConfigError::Missing)
}

fn valid_key(key: &str) -> bool {
    (16..=512).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

fn valid_model(model: &str) -> bool {
    (1..=96).contains(&model.len())
        && model.starts_with("gemini-")
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
}

fn model(home: &LatchHome) -> anyhow::Result<String> {
    let path = home.root().join("commit-message-model");
    let mut bytes = Vec::new();
    match std::fs::File::open(path) {
        Ok(file) => {
            file.take(128).read_to_end(&mut bytes)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DEFAULT_MODEL.into())
        }
        Err(_) => bail!("cannot read commit-message model"),
    }
    let value = String::from_utf8(bytes).context("invalid commit-message model")?;
    let value = value.trim();
    if !valid_model(value) {
        bail!("invalid commit-message model");
    }
    Ok(value.into())
}

/// Public command dispatch, kept independent of Remote Link and Overlord.
pub fn configure(action: &str, value: Option<&str>, stdin: bool) -> anyhow::Result<()> {
    let home = LatchHome::from_env()?;
    match action {
        "set-key" => {
            let key = read_key(stdin)?;
            #[cfg(target_os = "macos")]
            security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, key.as_bytes())
                .map_err(|_| anyhow::anyhow!("cannot store Gemini key in Keychain"))?;
            #[cfg(not(target_os = "macos"))]
            {
                drop(key);
                bail!("Gemini credentials require macOS Keychain");
            }
            #[cfg(target_os = "macos")]
            println!("Gemini credential saved in Keychain.");
        }
        "remove-key" => {
            #[cfg(target_os = "macos")]
            if let Err(error) =
                security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT)
            {
                if error.code() != -25300 {
                    bail!("cannot remove Gemini key from Keychain");
                }
            }
            println!("Gemini credential removed.");
        }
        "model" => {
            if let Some(value) = value {
                if !valid_model(value) {
                    bail!("expected a Gemini model identifier");
                }
                home.ensure()?;
                write_model(&home.root().join("commit-message-model"), value)?;
            }
            println!("{}", model(&home)?);
        }
        "status" => {
            headless_keychain()?;
            let state = match load(&home) {
                Ok(_) => "ready (local configuration only; provider access not verified)",
                Err(ConfigError::Missing) => "unconfigured",
                Err(ConfigError::Unavailable) => "unavailable (model or Keychain access)",
            };
            println!("{state}");
        }
        _ => bail!("unknown commit-message configuration command"),
    }
    Ok(())
}

fn write_model(path: &Path, model: &str) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(model.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result.context("cannot save commit-message model")
}

fn read_key(stdin: bool) -> anyhow::Result<Zeroizing<String>> {
    use std::os::fd::AsRawFd;
    let mut bytes = Zeroizing::new(Vec::new());
    if stdin {
        std::io::stdin()
            .lock()
            .take(514)
            .read_until(b'\n', &mut bytes)?;
    } else {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .context("no terminal; pipe the credential using --stdin")?;
        let fd = tty.as_raw_fd();
        // Restore echo on every ordinary exit, including a read error.
        struct Echo(i32, libc::termios);
        impl Drop for Echo {
            fn drop(&mut self) {
                unsafe {
                    libc::tcsetattr(self.0, libc::TCSANOW, &self.1);
                }
            }
        }
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            bail!("cannot secure terminal input");
        }
        let original = unsafe { original.assume_init() };
        let _echo = Echo(fd, original);
        let mut hidden = original;
        // Read control keys ourselves so Ctrl-C restores echo before returning.
        hidden.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG);
        hidden.c_cc[libc::VMIN] = 1;
        hidden.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &hidden) } != 0 {
            bail!("cannot disable terminal echo");
        }
        eprint!("Gemini API key: ");
        std::io::stderr().flush()?;
        loop {
            let mut byte = [0];
            tty.read_exact(&mut byte)?;
            match byte[0] {
                b'\r' | b'\n' => break,
                3 | 4 => {
                    eprintln!();
                    bail!("credential entry cancelled");
                }
                8 | 127 => {
                    bytes.pop();
                }
                value => bytes.push(value),
            }
            if bytes.len() > 512 {
                bail!("credential is too long");
            }
        }
        eprintln!();
    }
    let key = Zeroizing::new(
        String::from_utf8(bytes.to_vec())
            .map_err(|_| anyhow::anyhow!("invalid credential format"))?,
    );
    let key = Zeroizing::new(key.trim_end_matches(['\r', '\n']).to_owned());
    if !valid_key(&key) {
        bail!("invalid credential format");
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_configuration_reloads_model_without_mobile_or_gateway_restart() {
        let dir = tempfile::tempdir().unwrap();
        let home = LatchHome::new(dir.path().to_owned());
        assert_eq!(model(&home).unwrap(), "gemini-3.5-flash-lite");
        let path = home.root().join("commit-message-model");
        write_model(&path, "gemini-supported-override").unwrap();
        assert_eq!(model(&home).unwrap(), "gemini-supported-override");
        write_model(&path, DEFAULT_MODEL).unwrap();
        assert_eq!(model(&home).unwrap(), DEFAULT_MODEL);
        for value in [
            "",
            "https://example.invalid/key",
            "gemini-a/b",
            "gemini-a?key=secret",
            "gemini-a\nheader",
            "../gemini-a",
        ] {
            assert!(!valid_model(value));
            std::fs::write(&path, value).unwrap();
            assert!(model(&home).is_err());
        }
        for key in [
            "",
            "short",
            "a key with spaces",
            "abcdefghijklmnop\n",
            "abcdefghijklmnop:header",
        ] {
            assert!(!valid_key(key));
        }
        assert!(valid_key("fixture_only_key_1234"));
        assert!(valid_key("AQ.fixture_key-123456789"));
        assert!(!valid_key(&"a".repeat(513)));
        // Unit tests must never discover a real Keychain credential.
        assert!(matches!(load_key(), Err(ConfigError::Missing)));
    }
}
