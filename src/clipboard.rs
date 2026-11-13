//! System clipboard (plain text). Windows uses the Win32 clipboard directly;
//! elsewhere the text only lives inside the app.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::time::Duration;

    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(owner: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn GetClipboardData(format: u32) -> *mut c_void;
        fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalFree(mem: *mut c_void) -> *mut c_void;
        fn GlobalLock(mem: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(mem: *mut c_void) -> i32;
    }

    /// Opens the clipboard, retrying briefly while another program holds it.
    fn open() -> bool {
        for _ in 0..10 {
            // SAFETY: plain Win32 call; a null owner associates the clipboard with this task.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    pub fn get() -> Option<String> {
        // SAFETY: the clipboard is opened before and closed after reading; the
        // handle is locked while its UTF-16 contents are read up to the terminator.
        unsafe {
            if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 || !open() {
                return None;
            }
            let mut result = None;
            let handle = GetClipboardData(CF_UNICODETEXT);
            if !handle.is_null() {
                let ptr = GlobalLock(handle) as *const u16;
                if !ptr.is_null() {
                    let mut len = 0;
                    while *ptr.add(len) != 0 {
                        len += 1;
                    }
                    result = Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)));
                    GlobalUnlock(handle);
                }
            }
            CloseClipboard();
            result
        }
    }

    pub fn set(text: &str) -> bool {
        let wide: Vec<u16> = text.replace('\n', "\r\n").encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: the allocation is large enough for `wide`; ownership passes to
        // the system on success and is freed here otherwise.
        unsafe {
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
            if mem.is_null() {
                return false;
            }
            let ptr = GlobalLock(mem) as *mut u16;
            if ptr.is_null() {
                GlobalFree(mem);
                return false;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            GlobalUnlock(mem);
            if !open() {
                GlobalFree(mem);
                return false;
            }
            EmptyClipboard();
            let ok = !SetClipboardData(CF_UNICODETEXT, mem).is_null();
            if !ok {
                GlobalFree(mem);
            }
            CloseClipboard();
            ok
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::Mutex;
    static TEXT: Mutex<String> = Mutex::new(String::new());

    pub fn get() -> Option<String> {
        Some(TEXT.lock().ok()?.clone())
    }

    pub fn set(text: &str) -> bool {
        TEXT.lock().map(|mut t| *t = text.to_owned()).is_ok()
    }
}

/// Clipboard text with `\n` line breaks.
pub fn get() -> Option<String> {
    imp::get().map(|s| s.replace("\r\n", "\n"))
}

pub fn set(text: &str) -> bool {
    imp::set(text)
}
