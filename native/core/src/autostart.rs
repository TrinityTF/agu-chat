//! "Start with Windows": the app's entry in HKCU\Software\Microsoft\Windows\CurrentVersion\Run.
//! The setting is the source of truth; `sync` makes the registry match it (and repoints the entry at the
//! current exe, so it follows the install folder).

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const VALUE_NAME: &str = "Agu Chat";

/// The command line Windows runs at sign-in: the quoted exe path.
pub fn command_line(exe: &std::path::Path) -> String {
    format!("\"{}\"", exe.display())
}

/// Add or remove the Run entry. Errors are returned for the log; nothing here panics.
#[cfg(windows)]
pub fn sync(enabled: bool) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ};

    let mut key = HKEY::default();
    let r = unsafe { RegCreateKeyExW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), 0, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None) };
    if r != ERROR_SUCCESS {
        return Err(format!("open Run key: {r:?}"));
    }
    let res = if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string());
        exe.and_then(|exe| {
            let wide: Vec<u16> = command_line(&exe).encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
            let r = unsafe { RegSetValueExW(key, &HSTRING::from(VALUE_NAME), 0, REG_SZ, Some(bytes)) };
            if r == ERROR_SUCCESS { Ok(()) } else { Err(format!("set Run value: {r:?}")) }
        })
    } else {
        let r = unsafe { RegDeleteValueW(key, &HSTRING::from(VALUE_NAME)) };
        if r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND { Ok(()) } else { Err(format!("delete Run value: {r:?}")) }
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    res
}

/// macOS: a LaunchAgent in ~/Library/LaunchAgents that runs this exe at login.
#[cfg(target_os = "macos")]
pub fn sync(enabled: bool) -> Result<(), String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let path = std::path::PathBuf::from(home).join("Library/LaunchAgents/lol.agu.chat.plist");
    if !enabled {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("remove {}: {e}", path.display())),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let plist = launch_agent_plist(&exe.to_string_lossy());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, plist).map_err(|e| format!("write {}: {e}", path.display()))
}

/// The LaunchAgent: run the app once at login (no KeepAlive, so quitting it keeps it quit).
pub fn launch_agent_plist(exe: &str) -> String {
    let esc = exe.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>lol.agu.chat</string>
  <key>ProgramArguments</key><array><string>{esc}</string></array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#
    )
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn sync(_enabled: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn launch_agent_escapes_the_path() {
        let p = super::launch_agent_plist("/Applications/A & B.app/Contents/MacOS/Agu Chat");
        assert!(p.contains("<string>/Applications/A &amp; B.app/Contents/MacOS/Agu Chat</string>"));
        assert!(p.contains("<key>RunAtLoad</key><true/>"));
    }

    #[test]
    fn quotes_the_exe() {
        assert_eq!(super::command_line(std::path::Path::new(r"C:\Program Files\Agu Chat\Agu Chat.exe")), r#""C:\Program Files\Agu Chat\Agu Chat.exe""#);
    }

    /// Writes and then removes the real HKCU Run entry; run by hand: `cargo test -p agu-chat-core run_entry -- --ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn run_entry_round_trip() {
        let query = || std::process::Command::new("reg").args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "Agu Chat"]).output().unwrap();
        super::sync(true).unwrap();
        let out = query();
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(text.contains(&super::command_line(&std::env::current_exe().unwrap())), "{text}");
        super::sync(false).unwrap();
        assert!(!query().status.success());
        super::sync(false).unwrap(); // removing twice is fine
    }
}
