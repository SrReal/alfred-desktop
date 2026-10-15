use serde_json::Value;

fn validated(raw: &str) -> Result<String, String> {
    if raw.len() > 8192 || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("CHAT_LINK_INVALID".into());
    }
    let url = reqwest::Url::parse(raw).map_err(|_| "CHAT_LINK_INVALID")?;
    if !["http", "https"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !(raw.to_ascii_lowercase().starts_with("https://")
            || raw.to_ascii_lowercase().starts_with("http://"))
    {
        return Err("CHAT_LINK_HTTP_REQUIRED".into());
    }
    Ok(url.to_string())
}

pub async fn open(input: &Value) -> Result<(), String> {
    let url = validated(input["url"].as_str().ok_or("CHAT_LINK_REQUIRED")?)?;
    tauri::async_runtime::spawn_blocking(move || open_browser(&url))
        .await
        .map_err(|_| "CHAT_LINK_OPEN_FAILED")?
}

#[cfg(windows)]
fn open_browser(url: &str) -> Result<(), String> {
    use windows::{
        core::{w, PCWSTR},
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    };
    let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
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
        return Err("Windows no pudo abrir el navegador predeterminado.".into());
    }
    Ok(())
}
#[cfg(not(windows))]
fn open_browser(_: &str) -> Result<(), String> {
    Err("WINDOWS_REQUIRED".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_explicit_web_links_can_reach_the_os() {
        assert_eq!(
            validated("https://example.com/a?q=b#c").unwrap(),
            "https://example.com/a?q=b#c"
        );
        assert!(validated("http://localhost:1234/").is_ok());
        for raw in [
            "file:///C:/Windows/notepad.exe",
            "javascript:alert(1)",
            "data:text/html,test",
            "ms-settings:privacy",
            "https://user:pass@example.com",
            "https://example.com\n",
            "https:\\example.com",
            "//example.com",
            "C:\\test.exe",
        ] {
            assert!(validated(raw).is_err(), "{raw}");
        }
    }
}
