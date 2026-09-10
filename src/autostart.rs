//! Starting the tray at login, on request.
//!
//! Opt-in and reversible: a credential-holding tool that adds itself to login
//! items unasked is the kind of thing users resent discovering later.

use crate::error::{Error, Result};

// Only the Windows Run-key implementation keys its entry by name; the
// LaunchAgent/.desktop implementations hardcode "byte" directly in their
// generated file contents instead, so this constant would be dead code
// there.
#[cfg(windows)]
const ENTRY_NAME: &str = "byte";

/// Where byte would register itself, in words a user can check.
pub fn describe_location() -> String {
    #[cfg(windows)]
    {
        "the registry Run key (HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run)".to_string()
    }
    #[cfg(target_os = "macos")]
    {
        "a LaunchAgent in ~/Library/LaunchAgents".to_string()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "a .desktop entry in ~/.config/autostart".to_string()
    }
}

fn exe_path() -> Result<std::path::PathBuf> {
    std::env::current_exe().map_err(|source| Error::Io {
        path: std::path::PathBuf::from("<current_exe>"),
        source,
    })
}

#[cfg(windows)]
mod platform {
    use super::{ENTRY_NAME, Error, Result, exe_path};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn run_key(write: bool) -> Result<winreg::RegKey> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
        let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
        let access = if write {
            KEY_READ | KEY_WRITE
        } else {
            KEY_READ
        };
        hkcu.open_subkey_with_flags(RUN_KEY, access)
            .map_err(|source| Error::Io {
                path: std::path::PathBuf::from(RUN_KEY),
                source,
            })
    }

    pub fn status() -> Result<bool> {
        // A missing Run key is "nothing is registered", not a failure.
        //
        // Windows creates `...\CurrentVersion\Run` lazily, the first time
        // something registers a startup item -- so it is always present on a
        // desktop that has ever had one, and absent on a freshly created
        // profile that has not. `byte autostart status` must answer the
        // question on both, and the answer on the latter is `false`.
        //
        // This mirrors `disable`'s treatment of an absent *value* just
        // below; the key deserves the same reading. Found by CI on
        // `windows-latest`, whose runner profile has no Run key -- a state
        // no ordinary developer machine can reproduce, which is why it
        // survived local runs on Windows.
        let key = match run_key(false) {
            Ok(key) => key,
            Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        Ok(key.get_value::<String, _>(ENTRY_NAME).is_ok())
    }

    pub fn enable() -> Result<()> {
        let exe = exe_path()?;
        let key = run_key(true)?;
        key.set_value(ENTRY_NAME, &format!("\"{}\"", exe.display()))
            .map_err(|source| Error::Io {
                path: std::path::PathBuf::from(RUN_KEY),
                source,
            })
    }

    pub fn disable() -> Result<()> {
        let key = run_key(true)?;
        match key.delete_value(ENTRY_NAME) {
            Ok(()) => Ok(()),
            // Already absent is success, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::Io {
                path: std::path::PathBuf::from(RUN_KEY),
                source,
            }),
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{Error, Result, exe_path};

    fn entry_path() -> Result<std::path::PathBuf> {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| Error::Io {
                path: std::path::PathBuf::from("$HOME"),
                source: std::io::Error::other("HOME is not set"),
            })?;
        #[cfg(target_os = "macos")]
        {
            Ok(home.join("Library/LaunchAgents/fyi.jocke.byte.plist"))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(home.join(".config/autostart/byte.desktop"))
        }
    }

    fn contents(exe: &std::path::Path) -> String {
        #[cfg(target_os = "macos")]
        {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
                 \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <plist version=\"1.0\"><dict>\n\
                 <key>Label</key><string>fyi.jocke.byte</string>\n\
                 <key>ProgramArguments</key><array><string>{}</string></array>\n\
                 <key>RunAtLoad</key><true/>\n\
                 </dict></plist>\n",
                exe.display()
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            format!(
                "[Desktop Entry]\nType=Application\nName=byte\nExec={}\n\
                 X-GNOME-Autostart-enabled=true\n",
                exe.display()
            )
        }
    }

    pub fn status() -> Result<bool> {
        Ok(entry_path()?.exists())
    }

    pub fn enable() -> Result<()> {
        let path = entry_path()?;
        let exe = exe_path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| Error::Io {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        crate::atomic::write(&path, contents(&exe).as_bytes())
    }

    pub fn disable() -> Result<()> {
        let path = entry_path()?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::Io { path, source }),
        }
    }
}

/// Is byte currently set to start at login?
pub fn status() -> Result<bool> {
    platform::status()
}

/// Register byte to start at login.
pub fn enable() -> Result<()> {
    platform::enable()
}

/// Remove byte from login items. Already-absent is success.
pub fn disable() -> Result<()> {
    platform::disable()
}
