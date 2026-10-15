use super::*;
use crate::clipboard_native::{fixture_own_station, fixture_publish, Native};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::HANDLE,
        System::{StationsAndDesktops::*, Threading::GetCurrentThreadId},
        UI::WindowsAndMessaging::{CWF_CREATE_ONLY, WINSTA_ALL_ACCESS},
    },
};

struct Station {
    previous: HWINSTA,
    previous_desktop: HDESK,
    station: HWINSTA,
    desktop: HDESK,
}
impl Station {
    fn enter(owner: &str) -> Self {
        assert_eq!(uuid::Uuid::parse_str(owner).unwrap().to_string(), owner);
        let previous = unsafe { GetProcessWindowStation() }.unwrap();
        let previous_desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) }.unwrap();
        let name = format!("ap-clipboard-station-{owner}")
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let station = unsafe {
            CreateWindowStationW(
                PCWSTR(name.as_ptr()),
                CWF_CREATE_ONLY,
                WINSTA_ALL_ACCESS as u32,
                None,
            )
        }
        .or_else(|error| {
            // A standard user cannot name a station. An unnamed fallback is
            // permitted only if CREATE_ONLY proves it did not already exist.
            if error.code() == windows::core::HRESULT::from_win32(5) {
                unsafe {
                    CreateWindowStationW(
                        PCWSTR::null(),
                        CWF_CREATE_ONLY,
                        WINSTA_ALL_ACCESS as u32,
                        None,
                    )
                }
            } else {
                Err(error)
            }
        })
        .unwrap();
        assert_ne!(station, previous);
        unsafe { SetProcessWindowStation(station) }.unwrap();
        let access = DESKTOP_CREATEWINDOW.0
            | DESKTOP_CREATEMENU.0
            | DESKTOP_READOBJECTS.0
            | DESKTOP_WRITEOBJECTS.0
            | DESKTOP_ENUMERATE.0;
        let desktop = unsafe {
            CreateDesktopW(
                w!("OwnedClipboardDesktop"),
                PCWSTR::null(),
                None,
                DESKTOP_CONTROL_FLAGS::default(),
                access,
                None,
            )
        }
        .unwrap();
        unsafe { SetThreadDesktop(desktop) }.unwrap();
        assert_eq!(unsafe { GetProcessWindowStation() }.unwrap(), station);
        let mut actual = [0u16; 256];
        unsafe {
            GetUserObjectInformationW(
                HANDLE(station.0),
                UOI_NAME,
                Some(actual.as_mut_ptr().cast()),
                512,
                None,
            )
        }
        .unwrap();
        let name =
            String::from_utf16_lossy(&actual[..actual.iter().position(|c| *c == 0).unwrap()]);
        assert!(!name.eq_ignore_ascii_case("WinSta0"));
        assert!(!name.is_empty());
        fixture_own_station(station.0 as usize).unwrap();
        Self {
            previous,
            previous_desktop,
            station,
            desktop,
        }
    }
}
impl Drop for Station {
    fn drop(&mut self) {
        unsafe { SetThreadDesktop(self.previous_desktop) }.unwrap();
        unsafe { SetProcessWindowStation(self.previous) }.unwrap();
        unsafe { CloseDesktop(self.desktop) }.unwrap();
        unsafe { CloseWindowStation(self.station) }.unwrap();
    }
}
fn unicode(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}
pub(super) fn dib() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(40u32.to_le_bytes());
    bytes.extend(2i32.to_le_bytes());
    bytes.extend(2i32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(24u16.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(16u32.to_le_bytes());
    bytes.extend([0u8; 16]);
    bytes.extend([0, 0, 255, 0, 255, 0, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0]);
    bytes
}
#[tokio::test]
#[ignore = "explicit noninteractive owned window-station clipboard contract"]
async fn real_owned_station_clipboard_contract() {
    assert_eq!(
        std::env::var("ALFREDPET_NATIVE_CLIPBOARD").as_deref(),
        Ok("1")
    );
    let owner = storage::id();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "clipboard::tests::native::owned_station_child",
            "--ignored",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("ALFREDPET_CLIPBOARD_CHILD", owner)
        .creation_flags(0x08000000)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = child.spawn().unwrap();
    let job = crate::windows_native::ProcessJob::attach(&child).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(45), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    drop(job);
    let output = String::from_utf8(result.stdout).unwrap();
    let errors = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success(),
        "Owned clipboard child failed: {output}\n{errors}"
    );
    assert!(
        output.contains("CLIPBOARD_NATIVE_CONTRACT_PASS:"),
        "{output}"
    );
    print!("{output}");
}
#[test]
#[ignore = "internal subprocess; guard creates a distinct window station before all clipboard calls"]
fn owned_station_child() {
    let owner = std::env::var("ALFREDPET_CLIPBOARD_CHILD").expect("Owned child flag required");
    let station = Station::enter(&owner);
    let f = Fixture::new();
    let c = f.control();
    fixture_publish(&[(13, &unicode("Hola áé 😀 fixture"))]).unwrap();
    let observed = Native.read(&c).unwrap();
    assert_eq!(observed.text.as_deref(), Some("Hola áé 😀 fixture"));
    let old = observed.expected();
    let written = Native.write(&c, &old, "Explicit replacement 😀").unwrap();
    assert_eq!(written.text.as_deref(), Some("Explicit replacement 😀"));
    assert_eq!(
        Native.write(&c, &old, "Stale request").unwrap_err(),
        "CLIPBOARD_CHANGED_REINSPECT"
    );
    assert_eq!(
        Native.read(&c).unwrap().text.as_deref(),
        Some("Explicit replacement 😀")
    );
    fixture_publish(&[(13, &[0, 0xd8, 0, 0])]).unwrap();
    assert_eq!(Native.read(&c).unwrap_err(), "CLIPBOARD_UTF16_INVALID");
    fixture_publish(&[(13, &vec![1; MAX_BYTES + 4096])]).unwrap();
    assert_eq!(Native.read(&c).unwrap_err(), "CLIPBOARD_BYTE_LIMIT");
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
    let excluded =
        unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) };
    assert_ne!(excluded, 0);
    fixture_publish(&[
        (13, &unicode("Provider private fixture")),
        (excluded, &[1, 0, 0, 0]),
    ])
    .unwrap();
    let private = Native.read(&c).unwrap();
    assert!(private.excluded);
    assert!(private.text.is_none());
    assert_eq!(
        private.data(true).unwrap_err(),
        "SENSITIVE_CLIPBOARD_DENIED"
    );
    let nohistory = unsafe { RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")) };
    assert_ne!(nohistory, 0);
    fixture_publish(&[
        (13, &unicode("No history fixture")),
        (nohistory, &[0, 0, 0, 0]),
    ])
    .unwrap();
    assert!(Native.read(&c).unwrap().excluded);
    fixture_publish(&[(8, &dib())]).unwrap();
    let image = Native.read(&c).unwrap();
    assert!(image.image);
    let png = Native.image(&c, &image.expected()).unwrap();
    let decoded = image::load_from_memory(&png).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (2, 2));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let expected = image.expected();
    let input = json!({"expected":expected});
    let result = runtime.block_on(async {
        let review = review(&f.core, "clipboard.image", &input).await.unwrap();
        f.core
            .grants
            .lock()
            .unwrap()
            .insert(format!("clipboard.image:{review}"), Risk::Contextual);
        f.core
            .store
            .execute(
                "UPDATE module_configs SET enabled=0 WHERE module_id='vision'",
                &[],
            )
            .unwrap();
        f.core.execute("clipboard.image", input).await.unwrap()
    });
    assert_eq!(result["method"], "clipboard_dib");
    assert_eq!(result["coordinate_space"], "image_pixels");
    assert_eq!(result["rect"]["width"], 2);
    assert!(result["id"].is_string());
    assert_eq!(f.count(), 0);
    assert_eq!(
        f.core
            .store
            .query(
                "SELECT count(*) AS n FROM asset_records WHERE state='ready'",
                &[]
            )
            .unwrap()[0]["n"],
        1
    );
    drop(runtime);
    drop(c);
    f.finish();
    drop(station);
    println!(
        "CLIPBOARD_NATIVE_CONTRACT_PASS: {}",
        json!({"distinctNoninteractiveStation":true,"utf16Roundtrip":true,"staleWriteDenied":true,"providerExclusionHonored":true,"malformedAndOversizedTextDenied":true,"dibDecodedToPng":true,"privateAssetWithoutVision":true,"noAutomaticImageHistory":true,"userInteractiveClipboardAccessed":false})
    );
}
