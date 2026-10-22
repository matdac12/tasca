//! "Start with Windows": the same Startup shortcut `scripts/install.ps1`
//! creates, `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\Tasca.lnk`
//! -> this exe `--hidden`.
//!
//! `set_enabled` uses COM (IShellLink) and writes a file, so call it on a
//! worker thread, never on the UI thread. `enabled` is a single stat.

use std::path::{Path, PathBuf};

use windows::core::{w, Interface, HSTRING};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

use crate::log::log;

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\Tasca.lnk`.
pub fn link_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| {
        PathBuf::from(a)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup")
            .join("Tasca.lnk")
    })
}

/// The checkbox state: does the Startup shortcut exist?
pub fn enabled() -> bool {
    link_path().is_some_and(|p| p.is_file())
}

/// Create or remove the Startup shortcut. Worker thread only.
pub fn set_enabled(on: bool) -> Result<(), String> {
    let path = link_path().ok_or_else(|| "APPDATA is not set".to_string())?;
    if on {
        let r = create(&path);
        match &r {
            Ok(()) => log(format!("startup shortcut created: {}", path.display())),
            Err(e) => log(format!("startup shortcut failed: {e}")),
        }
        r
    } else {
        let r = remove(&path);
        match &r {
            Ok(()) => log(format!("startup shortcut removed: {}", path.display())),
            Err(e) => log(format!("startup shortcut removal failed: {e}")),
        }
        r
    }
}

fn remove(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn create(path: &Path) -> Result<(), String> {
    let _com = Com::new();
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("IShellLink: {e}"))?;
        let exe_w = HSTRING::from(exe.as_os_str());
        link.SetPath(&exe_w).map_err(|e| format!("SetPath: {e}"))?;
        link.SetArguments(w!("--hidden"))
            .map_err(|e| format!("SetArguments: {e}"))?;
        link.SetDescription(w!("Tasca"))
            .map_err(|e| format!("SetDescription: {e}"))?;
        if let Some(dir) = exe.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))
                .map_err(|e| format!("SetWorkingDirectory: {e}"))?;
        }
        link.SetIconLocation(&exe_w, 0)
            .map_err(|e| format!("SetIconLocation: {e}"))?;
        let file: IPersistFile = link.cast().map_err(|e| format!("IPersistFile: {e}"))?;
        file.Save(&HSTRING::from(path.as_os_str()), true)
            .map_err(|e| format!("Save: {e}"))?;
    }
    Ok(())
}

/// `CoInitializeEx`/`CoUninitialize` pairing for the worker thread.
struct Com(bool);

impl Com {
    fn new() -> Self {
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_lives_in_the_startup_folder() {
        if let Some(p) = link_path() {
            assert!(
                p.ends_with(r"Microsoft\Windows\Start Menu\Programs\Startup\Tasca.lnk"),
                "{}",
                p.display()
            );
        }
    }

    #[test]
    fn enabled_is_just_a_file_check() {
        // The real file is Mattia's installed Tasca: assert the API does not
        // panic, not what it returns.
        let _ = enabled();
    }
}
