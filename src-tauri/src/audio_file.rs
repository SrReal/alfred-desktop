pub async fn pick(window: &tauri::WebviewWindow) -> Result<Option<String>, String> {
    #[cfg(windows)]
    {
        let owner = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
        let (send, receive) = tokio::sync::oneshot::channel();
        // A dedicated STA owns the modal dialog; neither the WebView nor Tokio
        // workers are blocked or left with a different COM apartment model.
        std::thread::Builder::new()
            .name("audio-file-picker".into())
            .spawn(move || {
                let _ = send.send(native_pick(owner).map_err(|e| e.to_string()));
            })
            .map_err(|e| e.to_string())?;
        receive.await.map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Err("El selector de audio requiere Windows.".into())
    }
}

#[cfg(windows)]
fn native_pick(owner: isize) -> windows::core::Result<Option<String>> {
    use windows::{
        core::{w, HRESULT},
        Win32::{
            Foundation::{ERROR_CANCELLED, HWND},
            System::Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
            },
            UI::Shell::{
                Common::COMDLG_FILTERSPEC, FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST,
                FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST, FOS_STRICTFILETYPES,
                SIGDN_FILESYSPATH,
            },
        },
    };
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _apartment = Apartment;
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetTitle(w!("Seleccionar audio «Señor»"))?;
        dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
            pszName: w!("Audio WAV (*.wav)"),
            pszSpec: w!("*.wav"),
        }])?;
        dialog.SetDefaultExtension(w!("wav"))?;
        dialog.SetOptions(
            dialog.GetOptions()?
                | FOS_FORCEFILESYSTEM
                | FOS_FILEMUSTEXIST
                | FOS_PATHMUSTEXIST
                | FOS_NOCHANGEDIR
                | FOS_STRICTFILETYPES,
        )?;
        match dialog.Show(Some(HWND(owner as _))) {
            Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => Ok(None),
            Err(error) => Err(error),
            Ok(()) => {
                let path = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
                let result = path.to_string();
                CoTaskMemFree(Some(path.0.cast()));
                Ok(Some(result?))
            }
        }
    }
}
