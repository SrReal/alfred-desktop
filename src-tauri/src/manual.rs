use crate::runtime::Core;
use std::{io::Read, path::PathBuf, sync::Arc};

pub const FILE_NAME: &str = "AlfredPet-Manual-de-uso.pdf";

fn path(core: &Core) -> Result<PathBuf, String> {
    let mut path = core.resource.join("manual").join(FILE_NAME);
    if cfg!(debug_assertions) && !path.exists() {
        path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../output/pdf")
            .join(FILE_NAME);
    }
    let missing = "No se encuentra el manual incluido. Vuelve a instalar AlfredPet con el instalador actualizado.";
    if !path.is_file() {
        return Err(missing.into());
    }
    let mut file = std::fs::File::open(&path).map_err(|_| missing)?;
    let mut header = [0; 5];
    file.read_exact(&mut header).map_err(|_| missing)?;
    if &header != b"%PDF-" {
        return Err("El manual incluido no es un PDF válido. Reinstala AlfredPet.".into());
    }
    Ok(path)
}

pub async fn open(core: &Arc<Core>) -> Result<(), String> {
    let path = path(core)?;
    tauri::async_runtime::spawn_blocking(move || open_pdf(&path))
        .await
        .map_err(|_| "No se pudo iniciar la apertura del manual. Inténtalo de nuevo.")?
}

#[cfg(windows)]
fn open_pdf(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::{w, PCWSTR},
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // The target is the bundled PDF, never a path supplied by the frontend.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        return Err("Windows no pudo abrir el manual. Configura una aplicación predeterminada para archivos PDF e inténtalo de nuevo.".into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn open_pdf(_: &std::path::Path) -> Result<(), String> {
    Err("El manual se abre desde AlfredPet para Windows.".into())
}
