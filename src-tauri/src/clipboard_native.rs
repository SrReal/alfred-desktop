use crate::clipboard::{self, Adapter, Control, Expected, Snapshot};
pub(crate) struct Native;
#[cfg(windows)]
mod win {
    use super::*;
    use image::ImageDecoder;
    use std::io::Cursor;
    use windows::{
        core::w,
        Win32::{
            Foundation::{
                GetLastError, GlobalFree, SetLastError, ERROR_SUCCESS, HANDLE, HGLOBAL, HWND,
            },
            System::{DataExchange::*, Memory::*},
            UI::WindowsAndMessaging::*,
        },
    };
    struct Clipboard {
        window: HWND,
    }
    impl Clipboard {
        fn open() -> Result<Self, String> {
            let window = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("AlfredPet clipboard owner"),
                    WINDOW_STYLE::default(),
                    0,
                    0,
                    0,
                    0,
                    Some(HWND_MESSAGE),
                    None,
                    None,
                    None,
                )
            }
            .map_err(|_| "CLIPBOARD_OWNER_WINDOW")?;
            if unsafe { OpenClipboard(Some(window)) }.is_err() {
                let _ = unsafe { DestroyWindow(window) };
                return Err("CLIPBOARD_BUSY".into());
            }
            Ok(Self { window })
        }
    }
    impl Drop for Clipboard {
        fn drop(&mut self) {
            let _ = unsafe { CloseClipboard() };
            let _ = unsafe { DestroyWindow(self.window) };
        }
    }
    struct Memory(HGLOBAL);
    impl Memory {
        fn new(bytes: &[u8]) -> Result<Self, String> {
            let memory = Self(
                unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }
                    .map_err(|_| "CLIPBOARD_MEMORY")?,
            );
            let ptr = unsafe { GlobalLock(memory.0) };
            if ptr.is_null() {
                return Err("CLIPBOARD_MEMORY_LOCK".into());
            }
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast(), bytes.len());
            }
            let _ = unsafe { GlobalUnlock(memory.0) };
            Ok(memory)
        }
        fn publish(mut self, format: u32) -> Result<(), String> {
            unsafe { SetClipboardData(format, Some(HANDLE(self.0 .0))) }
                .map_err(|_| "CLIPBOARD_WRITE_FAILED_CONTENT_MAY_HAVE_CHANGED")?;
            self.0 = HGLOBAL::default();
            Ok(())
        }
    }
    impl Drop for Memory {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                let _ = unsafe { GlobalFree(Some(self.0)) };
            }
        }
    }
    fn bytes(format: u32, limit: usize) -> Result<Vec<u8>, String> {
        let handle =
            unsafe { GetClipboardData(format) }.map_err(|_| "CLIPBOARD_DATA_UNAVAILABLE")?;
        let memory = HGLOBAL(handle.0);
        let size = unsafe { GlobalSize(memory) };
        if size == 0 || size > limit {
            return Err("CLIPBOARD_BYTE_LIMIT".into());
        }
        let ptr = unsafe { GlobalLock(memory) };
        if ptr.is_null() {
            return Err("CLIPBOARD_MEMORY_LOCK".into());
        }
        let output = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), size) }.to_vec();
        let _ = unsafe { GlobalUnlock(memory) };
        Ok(output)
    }
    fn formats() -> Result<Vec<u32>, String> {
        let mut formats = Vec::new();
        let mut id = 0;
        loop {
            unsafe {
                SetLastError(ERROR_SUCCESS);
            }
            id = unsafe { EnumClipboardFormats(id) };
            if id == 0 {
                if unsafe { GetLastError() } != ERROR_SUCCESS {
                    return Err("CLIPBOARD_FORMAT_ENUMERATION".into());
                }
                break;
            }
            if formats.len() == 64 {
                return Err("CLIPBOARD_FORMAT_LIMIT".into());
            }
            formats.push(id);
        }
        Ok(formats)
    }
    fn available(format: u32) -> bool {
        unsafe { IsClipboardFormatAvailable(format) }.is_ok()
    }
    fn excluded() -> bool {
        let exclude =
            unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) };
        let history = unsafe { RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")) };
        if exclude == 0 || history == 0 {
            return true;
        }
        if available(exclude) {
            return true;
        }
        if available(history) {
            return bytes(history, 16).ok().and_then(|bytes| {
                bytes
                    .get(..4)
                    .and_then(|v| v.try_into().ok())
                    .map(u32::from_le_bytes)
            }) != Some(1);
        }
        false
    }
    fn snapshot(control: &Control) -> Result<Snapshot, String> {
        control.check()?;
        let sequence = unsafe { GetClipboardSequenceNumber() };
        let formats = formats()?;
        let excluded = excluded();
        let text = if !excluded && available(13) {
            let data = bytes(13, clipboard::MAX_BYTES)?;
            if data.len() % 2 != 0 {
                return Err("CLIPBOARD_UTF16_INVALID".into());
            }
            let units = data
                .chunks_exact(2)
                .map(|v| u16::from_le_bytes([v[0], v[1]]))
                .collect::<Vec<_>>();
            let end = units
                .iter()
                .position(|unit| *unit == 0)
                .ok_or("CLIPBOARD_TEXT_NOT_TERMINATED")?;
            let text = String::from_utf16(&units[..end]).map_err(|_| "CLIPBOARD_UTF16_INVALID")?;
            clipboard::text_valid(&text)?;
            Some(text)
        } else {
            None
        };
        let after = unsafe { GetClipboardSequenceNumber() };
        control.check()?;
        // Delayed rendering/synthesized formats can change the sequence during reads;
        // require reinspection instead of binding a write to a different snapshot.
        if after != sequence {
            return Err("CLIPBOARD_CHANGED_RETRY".into());
        }
        Ok(Snapshot {
            sequence,
            formats,
            text,
            image: available(8) || available(17) || available(2),
            excluded,
        })
    }
    impl Adapter for Native {
        fn sequence(&self) -> Result<u32, String> {
            Ok(unsafe { GetClipboardSequenceNumber() })
        }
        fn read(&self, control: &Control) -> Result<Snapshot, String> {
            control.check()?;
            let _clipboard = Clipboard::open()?;
            snapshot(control)
        }
        fn write(
            &self,
            control: &Control,
            expected: &Expected,
            text: &str,
        ) -> Result<Snapshot, String> {
            clipboard::text_valid(text)?;
            if clipboard::sensitive(text) {
                return Err("SENSITIVE_CLIPBOARD_DENIED".into());
            }
            control.check()?;
            let _clipboard = Clipboard::open()?;
            let before = snapshot(control)?;
            if before.expected() != *expected {
                return Err("CLIPBOARD_CHANGED_REINSPECT".into());
            }
            let raw = text
                .encode_utf16()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>();
            let replacement = Memory::new(&raw)?;
            control.check()?;
            unsafe { EmptyClipboard() }.map_err(|_| "CLIPBOARD_EMPTY_FAILED")?;
            replacement.publish(13)?;
            let result = snapshot(control)?;
            if result.text.as_deref() != Some(text) {
                return Err("CLIPBOARD_WRITE_READBACK_FAILED".into());
            }
            Ok(result)
        }
        fn image(&self, control: &Control, expected: &Expected) -> Result<Vec<u8>, String> {
            control.check()?;
            let dib = {
                let _clipboard = Clipboard::open()?;
                let before = snapshot(control)?;
                if before.expected() != *expected {
                    return Err("CLIPBOARD_CHANGED_REINSPECT".into());
                }
                if before.excluded
                    || before
                        .text
                        .as_ref()
                        .is_some_and(|t| clipboard::sensitive(t))
                {
                    return Err("SENSITIVE_CLIPBOARD_DENIED".into());
                }
                let format = if available(8) {
                    8
                } else if available(17) {
                    17
                } else {
                    return Err("CLIPBOARD_IMAGE_UNAVAILABLE".into());
                };
                let dib = bytes(format, 32 * 1024 * 1024)?;
                control.check()?;
                if unsafe { GetClipboardSequenceNumber() } != before.sequence {
                    return Err("CLIPBOARD_CHANGED_RETRY".into());
                }
                dib
            };
            decode_dib(control, dib)
        }
    }
    pub(crate) fn decode_dib(control: &Control, dib: Vec<u8>) -> Result<Vec<u8>, String> {
        control.check()?;
        if dib.len() > 32 * 1024 * 1024 {
            return Err("CLIPBOARD_IMAGE_BYTE_LIMIT".into());
        }
        let mut decoder = image::codecs::bmp::BmpDecoder::new_without_file_header(Cursor::new(dib))
            .map_err(|_| "CLIPBOARD_IMAGE_UNSUPPORTED")?;
        let (width, height) = decoder.dimensions();
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || u64::from(width) * u64::from(height) > 8 * 1024 * 1024
        {
            return Err("CLIPBOARD_IMAGE_DIMENSION_LIMIT".into());
        }
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(32 * 1024 * 1024);
        decoder
            .set_limits(limits)
            .map_err(|_| "CLIPBOARD_IMAGE_LIMIT")?;
        let decoded = image::DynamicImage::from_decoder(decoder)
            .map_err(|_| "CLIPBOARD_IMAGE_UNSUPPORTED")?;
        control.check()?;
        let mut png = Cursor::new(Vec::new());
        decoded
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|_| "CLIPBOARD_IMAGE_ENCODING")?;
        if png.get_ref().len() > 32 * 1024 * 1024 {
            return Err("CLIPBOARD_IMAGE_BYTE_LIMIT".into());
        }
        control.check()?;
        Ok(png.into_inner())
    }
    #[cfg(test)]
    static OWNED_STATION: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    #[cfg(test)]
    pub(crate) fn fixture_own_station(handle: usize) -> Result<(), String> {
        use windows::Win32::System::StationsAndDesktops::*;
        let station = unsafe { GetProcessWindowStation() }.map_err(|_| "FIXTURE_STATION")?;
        let mut name = [0u16; 256];
        unsafe {
            GetUserObjectInformationW(
                HANDLE(station.0),
                UOI_NAME,
                Some(name.as_mut_ptr().cast()),
                512,
                None,
            )
        }
        .map_err(|_| "FIXTURE_STATION")?;
        let name = String::from_utf16_lossy(
            &name[..name.iter().position(|c| *c == 0).ok_or("FIXTURE_STATION")?],
        );
        if name.is_empty() || name.eq_ignore_ascii_case("WinSta0") || station.0 as usize != handle {
            return Err("FIXTURE_INTERACTIVE_CLIPBOARD_DENIED".into());
        }
        OWNED_STATION
            .set(handle)
            .map_err(|_| "FIXTURE_ALREADY_INITIALIZED".into())
    }
    #[cfg(test)]
    pub(crate) fn fixture_publish(entries: &[(u32, &[u8])]) -> Result<(), String> {
        let station =
            unsafe { windows::Win32::System::StationsAndDesktops::GetProcessWindowStation() }
                .map_err(|_| "FIXTURE_STATION")?;
        if OWNED_STATION.get() != Some(&(station.0 as usize)) {
            return Err("FIXTURE_INTERACTIVE_CLIPBOARD_DENIED".into());
        }
        let _clipboard = Clipboard::open()?;
        unsafe { EmptyClipboard() }.map_err(|_| "CLIPBOARD_EMPTY_FAILED")?;
        for (format, bytes) in entries {
            Memory::new(bytes)?.publish(*format)?;
        }
        Ok(())
    }
}
#[cfg(not(windows))]
impl Adapter for Native {
    fn sequence(&self) -> Result<u32, String> {
        Err("WINDOWS_11_REQUIRED".into())
    }
    fn read(&self, _: &Control) -> Result<Snapshot, String> {
        Err("WINDOWS_11_REQUIRED".into())
    }
    fn write(&self, _: &Control, _: &Expected, _: &str) -> Result<Snapshot, String> {
        Err("WINDOWS_11_REQUIRED".into())
    }
    fn image(&self, _: &Control, _: &Expected) -> Result<Vec<u8>, String> {
        Err("WINDOWS_11_REQUIRED".into())
    }
}
#[cfg(all(test, windows))]
pub(crate) use win::{decode_dib, fixture_own_station, fixture_publish};
