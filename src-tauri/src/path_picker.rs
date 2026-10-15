use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    File,
    Folder,
    Save,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    mode: Mode,
    title: String,
    #[serde(default)]
    multiple: bool,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    default_name: String,
}

impl Options {
    pub fn parse(value: serde_json::Value) -> Result<Self, String> {
        let options: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if options.title.trim().is_empty()
            || options.title.len() > 240
            || options.title.chars().any(char::is_control)
            || options.extensions.len() > 16
            || options.extensions.iter().any(|extension| {
                extension.is_empty()
                    || extension.len() > 12
                    || !extension.chars().all(|c| c.is_ascii_alphanumeric())
            })
            || options.default_name.len() > 240
            || options
                .default_name
                .chars()
                .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
            || (options.multiple && options.mode == Mode::Save)
            || (options.mode == Mode::Folder && !options.extensions.is_empty())
        {
            return Err("PATH_PICKER_OPTIONS".into());
        }
        Ok(options)
    }
}

static OPEN: AtomicBool = AtomicBool::new(false);
struct DialogLease;
impl DialogLease {
    fn acquire() -> Result<Self, String> {
        OPEN.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Ya hay un selector abierto.".to_string())?;
        Ok(Self)
    }
}
impl Drop for DialogLease {
    fn drop(&mut self) {
        OPEN.store(false, Ordering::Release);
    }
}

pub async fn pick(
    window: &tauri::WebviewWindow,
    options: Options,
) -> Result<Option<Vec<String>>, String> {
    #[cfg(windows)]
    {
        let owner = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
        let lease = DialogLease::acquire()?;
        let (send, receive) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("path-picker".into())
            .spawn(move || {
                let result = native_pick(owner, options).map_err(|e| e.to_string());
                drop(lease);
                let _ = send.send(result);
            })
            .map_err(|e| e.to_string())?;
        receive.await.map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    {
        let _ = (window, options);
        Err("El selector de archivos requiere Windows.".into())
    }
}

#[cfg(windows)]
fn native_pick(owner: isize, options: Options) -> windows::core::Result<Option<Vec<String>>> {
    use windows::{
        core::{w, Interface, HRESULT, PCWSTR},
        Win32::{
            Foundation::{ERROR_CANCELLED, HWND},
            System::Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
            },
            UI::Shell::{
                Common::COMDLG_FILTERSPEC, FileOpenDialog, FileSaveDialog, IFileDialog,
                IFileOpenDialog, IFileSaveDialog, IShellItem, FOS_ALLOWMULTISELECT,
                FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_OVERWRITEPROMPT,
                FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
            },
        },
    };
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    fn path(item: &IShellItem) -> windows::core::Result<String> {
        unsafe {
            let value = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let result = value.to_string();
            CoTaskMemFree(Some(value.0.cast()));
            Ok(result?)
        }
    }
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _apartment = Apartment;
        let dialog: IFileDialog = if options.mode == Mode::Save {
            let save: IFileSaveDialog =
                CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
            save.cast()?
        } else {
            let open: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            open.cast()?
        };
        let title: Vec<u16> = options.title.encode_utf16().chain(Some(0)).collect();
        dialog.SetTitle(PCWSTR(title.as_ptr()))?;
        let mut flags =
            dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR | FOS_PATHMUSTEXIST;
        flags |= match options.mode {
            Mode::Folder => FOS_PICKFOLDERS,
            Mode::File => FOS_FILEMUSTEXIST,
            Mode::Save => FOS_OVERWRITEPROMPT,
        };
        if options.multiple {
            flags |= FOS_ALLOWMULTISELECT;
        }
        dialog.SetOptions(flags)?;
        let patterns = options
            .extensions
            .iter()
            .map(|e| format!("*.{e}"))
            .collect::<Vec<_>>()
            .join(";");
        let patterns: Vec<u16> = patterns.encode_utf16().chain(Some(0)).collect();
        if !options.extensions.is_empty() {
            dialog.SetFileTypes(&[
                COMDLG_FILTERSPEC {
                    pszName: w!("Archivos compatibles"),
                    pszSpec: PCWSTR(patterns.as_ptr()),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("Todos los archivos"),
                    pszSpec: w!("*.*"),
                },
            ])?;
        }
        let filename: Vec<u16> = options.default_name.encode_utf16().chain(Some(0)).collect();
        if options.mode == Mode::Save && !options.default_name.is_empty() {
            dialog.SetFileName(PCWSTR(filename.as_ptr()))?;
        }
        // Choosing a destination never writes a file or grants path permissions.
        match dialog.Show(Some(HWND(owner as _))) {
            Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => Ok(None),
            Err(error) => Err(error),
            Ok(()) if options.mode == Mode::Save => Ok(Some(vec![path(&dialog.GetResult()?)?])),
            Ok(()) => {
                let open: IFileOpenDialog = dialog.cast()?;
                let results = open.GetResults()?;
                let mut paths = Vec::new();
                for index in 0..results.GetCount()? {
                    paths.push(path(&results.GetItemAt(index)?)?);
                }
                Ok(Some(paths))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_file_folder_multi_and_save_contracts() {
        for value in [
            json!({"mode":"file","title":"Elegir imágenes","multiple":true,"extensions":["png","jpg"]}),
            json!({"mode":"folder","title":"Carpetas permitidas","multiple":true}),
            json!({"mode":"save","title":"Destino nuevo","default_name":"señor.txt"}),
        ] {
            assert!(Options::parse(value).is_ok());
        }
    }

    #[test]
    fn rejects_invalid_modes_filters_and_embedded_paths() {
        for value in [
            json!({"mode":"unknown","title":"Elegir"}),
            json!({"mode":"save","title":"Elegir","multiple":true}),
            json!({"mode":"folder","title":"Elegir","extensions":["png"]}),
            json!({"mode":"file","title":"Elegir","extensions":["png;*.exe"]}),
            json!({"mode":"file","title":"Elegir\u{0}"}),
            json!({"mode":"save","title":"Elegir","default_name":"C:\\test.txt"}),
            json!({"mode":"file","title":"Elegir","grant_permissions":true}),
        ] {
            assert!(Options::parse(value).is_err());
        }
    }

    #[test]
    fn simultaneous_dialog_is_rejected_and_lease_is_released() {
        let first = DialogLease::acquire().unwrap();
        assert!(DialogLease::acquire().is_err());
        drop(first);
        assert!(DialogLease::acquire().is_ok());
    }
}
