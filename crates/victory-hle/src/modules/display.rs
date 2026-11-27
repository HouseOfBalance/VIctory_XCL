//! The display and what the system shell does for a title: HDR mode, screen
//! timeout deferrals, launching URIs, screenshots.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::xgameruntime::{
    Guid, HRESULT, Interface, Methods, NOT_SUPPORTED, Object, S_OK, add_ref, missing, put, text,
};

/// HDR mode; asked for with this interface id since 2020.
pub const HDR_PROVIDER: Guid = Guid::new(
    0x03f0_fe74,
    0xfdd9,
    0x4e5c,
    [0xb6, 0x30, 0xf9, 0x33, 0x9c, 0x47, 0xac, 0xc5],
);
pub const HDR_IID: Guid = Guid::new(
    0x35f0_7670,
    0x706e,
    0x4bfb,
    [0x94, 0x76, 0x09, 0x07, 0x98, 0xc5, 0xeb, 0xf3],
);
/// Launching URIs and screen timeout deferrals; the interface id is the
/// provider's.
pub const SHELL: Guid = Guid::new(
    0x1b33_9674,
    0x328d,
    0x4283,
    [0xa2, 0x00, 0x31, 0x71, 0xf1, 0x8d, 0x36, 0x39],
);
/// Screenshots and clips.
pub const CAPTURE_PROVIDER: Guid = Guid::new(
    0xa4f1_aee2,
    0x4bf1,
    0x4485,
    [0xb0, 0x08, 0xa7, 0xc2, 0x6d, 0x52, 0xac, 0x27],
);
/// As the title (2026) asks for it; it calls 0x28 (take a screenshot) with
/// the public API's arguments.
pub const CAPTURE_IID_2026: Guid = Guid::new(
    0x22e6_72d7,
    0xb4e3,
    0x406c,
    [0xbd, 0x50, 0x8f, 0x0d, 0x25, 0x23, 0x6f, 0x9e],
);

pub static HDR_OBJECT: Object = Object::new("hdr", &HDR_VTABLE);
pub static SHELL_OBJECT: Object = Object::new("shell", &SHELL_VTABLE);
pub static CAPTURE_OBJECT: Object = Object::new("capture", &CAPTURE_VTABLE);

pub const INTERFACES: &[Interface] = &[
    (HDR_PROVIDER, HDR_IID, &HDR_OBJECT),
    (SHELL, SHELL, &SHELL_OBJECT),
    (CAPTURE_PROVIDER, CAPTURE_IID_2026, &CAPTURE_OBJECT),
];

type This = *const Object;

/// `XDisplayHdrModeInfo`: tone-map luminance values in nits.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HdrModeInfo {
    pub min_tone_map_luminance: f32,
    pub max_tone_map_luminance: f32,
    pub max_full_frame_tone_map_luminance: f32,
}
const _: () = assert!(size_of::<HdrModeInfo>() == 12);

/// `XDisplayTryEnableHdrMode`: returns `1` (`Enabled`) with the host output's
/// HDR luminance range when `info` is provided and the primary display is in
/// HDR mode; otherwise `2` (`Disabled`, SDR presentation).
extern "system" fn try_enable_hdr(_: This, preference: u32, info: *mut c_void) -> u32 {
    const ENABLED: u32 = 1;
    const DISABLED: u32 = 2;
    if info.is_null() {
        let _ = victory_gpu::set_hdr_mode(false);
        tracing::info!(preference, "HDR mode: disabled (no info output)");
        return DISABLED;
    }
    if let Some(hdr) = victory_gpu::set_hdr_mode(true) {
        let out = HdrModeInfo {
            min_tone_map_luminance: hdr.min_luminance,
            max_tone_map_luminance: hdr.max_luminance,
            max_full_frame_tone_map_luminance: hdr.max_full_frame_luminance,
        };
        // SAFETY: checked non-null; the title passes room for `XDisplayHdrModeInfo`.
        unsafe { info.cast::<HdrModeInfo>().write_unaligned(out) };
        tracing::info!(
            preference,
            min = out.min_tone_map_luminance,
            max = out.max_tone_map_luminance,
            full_frame = out.max_full_frame_tone_map_luminance,
            "HDR mode: enabled"
        );
        ENABLED
    } else {
        tracing::info!(preference, "HDR mode: disabled (SDR presentation)");
        DISABLED
    }
}

/// Nothing launches URIs for the title: no store, browser or console app
/// exists here, and the console's are online services.
extern "system" fn launch_uri(_: This, _user: *const c_void, uri: *const c_char) -> HRESULT {
    tracing::info!(uri = text(uri).unwrap_or_default(), "URI not launched");
    NOT_SUPPORTED
}

/// A deferral keeps the host display on, as on the console, through a
/// power request; the handle is that request.
#[cfg(windows)]
extern "system" fn acquire_deferral(_: This, out: *mut *mut c_void) -> HRESULT {
    use windows_sys::Win32::System::Power::{
        PowerCreateRequest, PowerRequestDisplayRequired, PowerSetRequest,
    };
    use windows_sys::Win32::System::Threading::{
        POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT,
    };
    if out.is_null() {
        return super::xasync::E_INVALIDARG;
    }
    let mut reason: Vec<u16> = "Title display deferral\0".encode_utf16().collect();
    let mut context = REASON_CONTEXT {
        Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
        ..Default::default()
    };
    context.Reason.SimpleReasonString = reason.as_mut_ptr();
    // SAFETY: a valid context; the string is copied by the call.
    let request = unsafe { PowerCreateRequest(&context) };
    if request as isize == -1 {
        return last_error();
    }
    // SAFETY: the request we just created.
    if unsafe { PowerSetRequest(request, PowerRequestDisplayRequired) } == 0 {
        let hr = last_error();
        // SAFETY: as above; not handed out.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(request) };
        return hr;
    }
    put(out, request)
}

#[cfg(windows)]
extern "system" fn close_deferral(_: This, handle: *mut c_void) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Power::{PowerClearRequest, PowerRequestDisplayRequired};
    if !handle.is_null() {
        // SAFETY: a request from `acquire_deferral`.
        unsafe {
            PowerClearRequest(handle, PowerRequestDisplayRequired);
            CloseHandle(handle);
        }
    }
}

#[cfg(not(windows))]
extern "system" fn acquire_deferral(_: This, _: *mut *mut c_void) -> HRESULT {
    NOT_SUPPORTED
}

#[cfg(not(windows))]
extern "system" fn close_deferral(_: This, _: *mut c_void) {}

#[cfg(windows)]
fn last_error() -> HRESULT {
    // SAFETY: no preconditions.
    let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    (0x8007_0000 | (code & 0xffff)) as i32
}

/// `XAppCaptureTakeScreenshotResult`: 256-byte local capture ID plus format flags.
#[repr(C)]
#[derive(Clone, Copy)]
struct ScreenshotResult {
    local_id: [u8; 256],
    available_formats: u16,
}
const _: () = assert!(size_of::<ScreenshotResult>() == 258);

static SCREENSHOT_SEQ: AtomicU64 = AtomicU64::new(0);

fn screenshot_dir() -> PathBuf {
    std::env::var_os("VICTORY_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("victory-screenshots"))
}

fn png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend((data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend(kind);
    png.extend(data);
    let mut crc = !0u32;
    for &b in &png[start..] {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    png.extend((!crc).to_be_bytes());
}

fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgba.len() < w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for y in 0..h {
        raw.push(0); // filter: None
        raw.extend_from_slice(&rgba[y * w * 4..(y + 1) * w * 4]);
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]); // 8-bit RGBA
    png_chunk(&mut png, b"IHDR", &ihdr);
    let mut buf = vec![0u8; zlib_rs::compress_bound(raw.len())];
    let (compressed, rc) =
        zlib_rs::compress_slice(&mut buf, &raw, zlib_rs::DeflateConfig::new(4));
    if rc != zlib_rs::ReturnCode::Ok {
        return None;
    }
    png_chunk(&mut png, b"IDAT", compressed);
    png_chunk(&mut png, b"IEND", &[]);
    Some(png)
}

fn write_screenshot_into(
    folder: &Path,
    frame: &victory_gpu::PresentedFrame,
    result: *mut c_void,
) -> HRESULT {
    let Some(png) = encode_rgba_png(frame.width, frame.height, &frame.rgba) else {
        return NOT_SUPPORTED;
    };
    if std::fs::create_dir_all(folder).is_err() {
        return NOT_SUPPORTED;
    }
    let seq = SCREENSHOT_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let id = format!("victory-shot-{seq}");
    let path = folder.join(format!("{id}.png"));
    if let Err(e) = std::fs::write(&path, png) {
        tracing::warn!(error = %e, path = %path.display(), "screenshot write failed");
        return NOT_SUPPORTED;
    }
    if !result.is_null() {
        let mut out = ScreenshotResult {
            local_id: [0; 256],
            available_formats: if frame.hdr { 3 } else { 1 },
        };
        let bytes = id.as_bytes();
        let len = bytes.len().min(out.local_id.len() - 1);
        out.local_id[..len].copy_from_slice(&bytes[..len]);
        // SAFETY: checked non-null; the caller passes room for `XAppCaptureTakeScreenshotResult`.
        unsafe { result.cast::<ScreenshotResult>().write_unaligned(out) };
    }
    tracing::info!(path = %path.display(), width = frame.width, height = frame.height, "screenshot saved");
    S_OK
}

extern "system" fn take_screenshot(_: This, _user: *const c_void, result: *mut c_void) -> HRESULT {
    let dir = screenshot_dir();
    if !crate::runtime::outside_repo(&dir) {
        return NOT_SUPPORTED;
    }
    let Some(frame) = victory_gpu::last_frame() else {
        tracing::info!("screenshot not taken: no frame presented yet");
        return NOT_SUPPORTED;
    };
    write_screenshot_into(&dir, &frame, result)
}

static HDR_VTABLE: Methods<5> = Methods([
    missing::<0> as *const (),
    add_ref as *const (),
    add_ref as *const (),
    missing::<3> as *const (),
    try_enable_hdr as *const (), // 0x20
]);

static SHELL_VTABLE: Methods<6> = Methods([
    missing::<0> as *const (),
    add_ref as *const (),
    add_ref as *const (),
    launch_uri as *const (),       // 0x18
    acquire_deferral as *const (), // 0x20
    close_deferral as *const (),   // 0x28
]);

static CAPTURE_VTABLE: Methods<6> = Methods([
    missing::<0> as *const (),
    add_ref as *const (),
    add_ref as *const (),
    missing::<3> as *const (),    // 0x18 diagnostic screenshot
    missing::<4> as *const (),    // 0x20 diagnostic clip
    take_screenshot as *const (), // 0x28
]);

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::modules::xgameruntime::method;

    #[test]
    fn sdr_and_display_deferrals() {
        type Hdr = extern "system" fn(This, u32, *mut c_void) -> u32;
        assert_eq!(
            method::<Hdr>(&HDR_OBJECT, 0x20)(&HDR_OBJECT, 0, std::ptr::null_mut()),
            2
        );
        let mut hdr_info = HdrModeInfo::default();
        let res = method::<Hdr>(&HDR_OBJECT, 0x20)(
            &HDR_OBJECT,
            0,
            (&raw mut hdr_info).cast(),
        );
        assert!(matches!(res, 1 | 2));
        if res == 1 {
            assert!(hdr_info.max_tone_map_luminance >= 80.0);
        }

        type Acquire = extern "system" fn(This, *mut *mut c_void) -> HRESULT;
        type Close = extern "system" fn(This, *mut c_void);
        let mut handle = std::ptr::null_mut();
        match method::<Acquire>(&SHELL_OBJECT, 0x20)(&SHELL_OBJECT, &mut handle) {
            S_OK => {
                assert!(!handle.is_null());
                method::<Close>(&SHELL_OBJECT, 0x28)(&SHELL_OBJECT, handle);
            }
            NOT_SUPPORTED => assert!(handle.is_null()),
            hr if hr < 0 => assert!(handle.is_null()),
            hr => panic!("unexpected display deferral HRESULT {:#010x}", hr as u32),
        }
    }

    #[test]
    fn screenshot_saves_valid_png_and_reports_local_id() {
        type Shot = extern "system" fn(This, *const c_void, *mut c_void) -> HRESULT;
        assert_eq!(
            method::<Shot>(&CAPTURE_OBJECT, 0x28)(
                &CAPTURE_OBJECT,
                std::ptr::null(),
                std::ptr::null_mut()
            ),
            NOT_SUPPORTED
        );
        let dir = tempfile::tempdir().unwrap();
        let frame = victory_gpu::PresentedFrame {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
            hdr: true,
        };
        let mut out = ScreenshotResult {
            local_id: [0; 256],
            available_formats: 0,
        };
        assert_eq!(
            write_screenshot_into(dir.path(), &frame, (&raw mut out).cast()),
            S_OK
        );
        assert_eq!(out.available_formats, 3);
        let len = out.local_id.iter().position(|&b| b == 0).unwrap();
        let id = std::str::from_utf8(&out.local_id[..len]).unwrap();
        let saved = std::fs::read(dir.path().join(format!("{id}.png"))).unwrap();
        assert!(saved.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(saved.ends_with(b"\x00\x00\x00\x00IEND\xaeB`\x82"));
    }
}
