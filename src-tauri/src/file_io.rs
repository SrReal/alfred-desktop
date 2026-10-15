//! Native I/O errors keep a stable operation name and the OS code. Test faults
//! belong to one Control, never an environment/global production switch.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Point {
    CopyRead,
    CopyWrite,
    CopyFlush,
    CheckpointRead,
    CheckpointWrite,
    CheckpointFlush,
    Write,
    Flush,
    UndoRestore,
    UndoFlush,
}
impl Point {
    fn name(self) -> &'static str {
        match self {
            Self::CopyRead => "TREE_COPY_READ",
            Self::CopyWrite => "TREE_COPY_WRITE",
            Self::CopyFlush => "TREE_COPY_FLUSH",
            Self::CheckpointRead => "CHECKPOINT_READ",
            Self::CheckpointWrite => "CHECKPOINT_WRITE",
            Self::CheckpointFlush => "CHECKPOINT_FLUSH",
            Self::Write => "FILE_WRITE",
            Self::Flush => "FILE_FLUSH",
            Self::UndoRestore => "UNDO_RESTORE",
            Self::UndoFlush => "UNDO_FLUSH",
        }
    }
}
fn error(point: Point, error: std::io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => format!("{}: os={code}", point.name()),
        None => point.name().into(),
    }
}

#[cfg(all(test, windows))]
pub(super) struct Fault {
    pub point: Point,
    pub remaining: usize,
    pub os_error: i32,
    pub fired: bool,
}
#[cfg(all(test, windows))]
fn allowance(
    control: &Control,
    point: Point,
    length: usize,
) -> Result<(usize, Option<i32>), String> {
    let mut fault = control.io_fault.lock().map_err(|_| "FILES_FAULT_LOCK")?;
    if let Some(fault) = fault
        .as_mut()
        .filter(|fault| fault.point == point && !fault.fired)
    {
        let allowed = length.min(fault.remaining);
        fault.remaining -= allowed;
        if allowed < length {
            fault.fired = true;
            return Ok((allowed, Some(fault.os_error)));
        }
    }
    Ok((length, None))
}
pub(super) fn read(
    file: &mut File,
    buffer: &mut [u8],
    point: Point,
    _control: &Control,
) -> Result<usize, String> {
    #[cfg(all(test, windows))]
    {
        let (allowed, injected) = allowance(_control, point, buffer.len())?;
        if let Some(code) = injected {
            // Read faults fail a native read boundary; previously completed
            // chunks in a checkpoint remain real bytes on the test filesystem.
            let _ = allowed;
            return Err(error(point, std::io::Error::from_raw_os_error(code)));
        }
    }
    file.read(buffer).map_err(|failure| error(point, failure))
}
pub(super) fn write_all(
    file: &mut File,
    bytes: &[u8],
    point: Point,
    _control: &Control,
) -> Result<(), String> {
    #[cfg(all(test, windows))]
    {
        let (allowed, injected) = allowance(_control, point, bytes.len())?;
        if let Some(code) = injected {
            file.write_all(&bytes[..allowed])
                .map_err(|failure| error(point, failure))?;
            return Err(error(point, std::io::Error::from_raw_os_error(code)));
        }
    }
    file.write_all(bytes)
        .map_err(|failure| error(point, failure))
}
pub(super) fn flush(file: &File, point: Point, _control: &Control) -> Result<(), String> {
    #[cfg(all(test, windows))]
    {
        let (_, injected) = allowance(_control, point, 1)?;
        if let Some(code) = injected {
            return Err(error(point, std::io::Error::from_raw_os_error(code)));
        }
    }
    file.sync_all().map_err(|failure| error(point, failure))
}

#[cfg(all(test, windows))]
#[path = "file_io_tests.rs"]
mod tests;
