//! Native dialogs: open / save file pickers, the unsaved-changes question and
//! opening a file in its default program. Windows uses the Win32 common
//! dialogs directly (like [`crate::clipboard`]); elsewhere pickers return
//! `None` and questions answer "discard" so the app stays usable.

use std::path::{Path, PathBuf};

/// Answer to "Save changes to …?".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SaveChoice {
    Save,
    Discard,
    Cancel,
}

/// Native window handle used as the owner of dialogs (0 when unknown).
pub type Owner = isize;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::{OsStr, OsString, c_void};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[repr(C)]
    struct OpenFileName {
        struct_size: u32,
        owner: isize,
        instance: *mut c_void,
        filter: *const u16,
        custom_filter: *mut u16,
        max_cust_filter: u32,
        filter_index: u32,
        file: *mut u16,
        max_file: u32,
        file_title: *mut u16,
        max_file_title: u32,
        initial_dir: *const u16,
        title: *const u16,
        flags: u32,
        file_offset: u16,
        file_extension: u16,
        def_ext: *const u16,
        cust_data: isize,
        hook: *mut c_void,
        template_name: *const u16,
        reserved_ptr: *mut c_void,
        reserved: u32,
        flags_ex: u32,
    }

    const OFN_OVERWRITEPROMPT: u32 = 0x2;
    const OFN_NOCHANGEDIR: u32 = 0x8;
    const OFN_PATHMUSTEXIST: u32 = 0x800;
    const OFN_FILEMUSTEXIST: u32 = 0x1000;
    const OFN_EXPLORER: u32 = 0x80000;
    const MB_YESNOCANCEL: u32 = 0x3;
    const MB_ICONWARNING: u32 = 0x30;
    const IDYES: i32 = 6;
    const IDNO: i32 = 7;
    const SW_SHOWNORMAL: i32 = 1;

    #[link(name = "comdlg32")]
    unsafe extern "system" {
        fn GetOpenFileNameW(ofn: *mut OpenFileName) -> i32;
        fn GetSaveFileNameW(ofn: *mut OpenFileName) -> i32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(owner: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            owner: isize,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }

    fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(Some(0)).collect()
    }

    /// `"SysML files\0*.sysml\0All files\0*.*\0\0"`
    fn filter(parts: &[&str]) -> Vec<u16> {
        let mut f = Vec::new();
        for part in parts {
            f.extend(part.encode_utf16());
            f.push(0);
        }
        f.push(0);
        f
    }

    const SYSML: [&str; 4] = ["SysML files (*.sysml)", "*.sysml", "All files (*.*)", "*.*"];
    const FONTS: [&str; 4] = ["Fonts (*.ttf, *.otf, *.ttc)", "*.ttf;*.otf;*.ttc", "All files (*.*)", "*.*"];
    const PNG: [&str; 2] = ["PNG images (*.png)", "*.png"];

    fn run(owner: Owner, title: &str, initial: Option<&Path>, save: bool, parts: &[&str], def_ext: &str) -> Option<PathBuf> {
        let mut buffer = vec![0u16; 4096];
        let mut dir = None;
        if let Some(p) = initial {
            if p.is_dir() {
                dir = Some(wide(p));
            } else {
                if let Some(name) = p.file_name() {
                    let name: Vec<u16> = name.encode_wide().collect();
                    let n = name.len().min(buffer.len() - 1);
                    buffer[..n].copy_from_slice(&name[..n]);
                }
                dir = p.parent().filter(|d| d.is_dir()).map(wide);
            }
        }
        let filter = filter(parts);
        let title = wide(title);
        let ext = wide(def_ext);
        let mut flags = OFN_EXPLORER | OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST;
        flags |= if save { OFN_OVERWRITEPROMPT } else { OFN_FILEMUSTEXIST };
        let mut ofn = OpenFileName {
            struct_size: size_of::<OpenFileName>() as u32,
            owner,
            instance: std::ptr::null_mut(),
            filter: filter.as_ptr(),
            custom_filter: std::ptr::null_mut(),
            max_cust_filter: 0,
            filter_index: 1,
            file: buffer.as_mut_ptr(),
            max_file: buffer.len() as u32,
            file_title: std::ptr::null_mut(),
            max_file_title: 0,
            initial_dir: dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
            title: title.as_ptr(),
            flags,
            file_offset: 0,
            file_extension: 0,
            def_ext: ext.as_ptr(),
            cust_data: 0,
            hook: std::ptr::null_mut(),
            template_name: std::ptr::null(),
            reserved_ptr: std::ptr::null_mut(),
            reserved: 0,
            flags_ex: 0,
        };
        // SAFETY: every pointer in `ofn` refers to a live, NUL-terminated buffer
        // owned by this function; `file` has `max_file` u16s of room.
        let ok = unsafe { if save { GetSaveFileNameW(&mut ofn) } else { GetOpenFileNameW(&mut ofn) } };
        if ok == 0 {
            return None;
        }
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(PathBuf::from(OsString::from_wide(&buffer[..len])))
    }

    pub fn open_file(owner: Owner, initial: Option<&Path>) -> Option<PathBuf> {
        run(owner, "Open SysML model", initial, false, &SYSML, "sysml")
    }

    pub fn save_file(owner: Owner, initial: Option<&Path>) -> Option<PathBuf> {
        run(owner, "Save SysML model as", initial, true, &SYSML, "sysml")
    }

    pub fn pick_font(owner: Owner, initial: Option<&Path>) -> Option<PathBuf> {
        run(owner, "Choose a font file", initial, false, &FONTS, "ttf")
    }

    pub fn save_png(owner: Owner, initial: Option<&Path>) -> Option<PathBuf> {
        run(owner, "Export diagram as PNG", initial, true, &PNG, "png")
    }

    pub fn ask_save(owner: Owner, file: &str) -> SaveChoice {
        let text = wide(format!("Save changes to {file}?"));
        let caption = wide("EagleView");
        // SAFETY: both strings are NUL-terminated and outlive the call.
        match unsafe { MessageBoxW(owner, text.as_ptr(), caption.as_ptr(), MB_YESNOCANCEL | MB_ICONWARNING) } {
            IDYES => SaveChoice::Save,
            IDNO => SaveChoice::Discard,
            _ => SaveChoice::Cancel,
        }
    }

    pub fn open_external(owner: Owner, path: &Path) -> bool {
        let op = wide("open");
        let file = wide(path);
        // SAFETY: NUL-terminated strings that outlive the call; null params/dir are allowed.
        let r = unsafe { ShellExecuteW(owner, op.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
        if r > 32 {
            return true;
        }
        // No program is associated with the extension: fall back to Notepad.
        std::process::Command::new("notepad.exe").arg(path).spawn().is_ok()
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn open_file(_: Owner, _: Option<&Path>) -> Option<PathBuf> {
        None
    }
    pub fn save_file(_: Owner, _: Option<&Path>) -> Option<PathBuf> {
        None
    }
    pub fn pick_font(_: Owner, _: Option<&Path>) -> Option<PathBuf> {
        None
    }
    pub fn save_png(_: Owner, _: Option<&Path>) -> Option<PathBuf> {
        None
    }
    pub fn ask_save(_: Owner, _: &str) -> SaveChoice {
        SaveChoice::Discard
    }
    pub fn open_external(_: Owner, path: &Path) -> bool {
        std::process::Command::new("xdg-open").arg(path).spawn().is_ok()
    }
}

pub use imp::{ask_save, open_external, open_file, pick_font, save_file, save_png};

/// Whether the native pickers exist on this platform.
pub const HAS_PICKERS: bool = cfg!(windows);
