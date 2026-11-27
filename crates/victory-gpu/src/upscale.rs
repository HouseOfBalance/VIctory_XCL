//! Display upscaling and super-resolution at presentation time:
//! - Dynamic NVIDIA NGX DLSS Super Resolution (`_nvngx.dll` / `nvngx_dlss.dll`
//!   loaded at run time from the installed NVIDIA driver or `VICTORY_NGX_DLL`,
//!   with zero bundled proprietary binaries).
//! - Universal D3D12 5-tap Catmull-Rom bicubic + Contrast-Adaptive Sharpening
//!   (RCAS-inspired anti-ringing lobe) fullscreen graphics pass that scales
//!   any title render resolution (1080p, 1440p, dynamic resolution) to the
//!   window or `VICTORY_OUTPUT_RES` (e.g. `3840x2160`) at 120/144+ FPS on
//!   AMD, NVIDIA, Intel and WARP adapters.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlssQuality {
    UltraPerformance,
    Performance,
    Balanced,
    Quality,
    Dlaa,
}

impl DlssQuality {
    /// NVIDIA NGX `NVSDK_NGX_PerfQuality_Value` enum code.
    pub const fn ngx_code(self) -> i32 {
        match self {
            DlssQuality::Performance => 0,
            DlssQuality::Balanced => 1,
            DlssQuality::Quality => 2,
            DlssQuality::UltraPerformance => 3,
            DlssQuality::Dlaa => 5,
        }
    }

    /// Recommended output scale factor from render resolution when no explicit
    /// `VICTORY_OUTPUT_RES` is set.
    pub const fn scale_num_den(self) -> (u32, u32) {
        match self {
            DlssQuality::UltraPerformance => (3, 1),
            DlssQuality::Performance => (2, 1),
            DlssQuality::Balanced => (17, 10),
            DlssQuality::Quality => (3, 2),
            DlssQuality::Dlaa => (1, 1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpscaleMode {
    /// Zero-copy 1:1 when resolutions match; spatial bicubic + CAS when scaled.
    Auto,
    /// Always run bicubic + Contrast-Adaptive Sharpening pass.
    Cas,
    /// NVIDIA NGX DLSS Super Resolution with spatial CAS fallback.
    Dlss(DlssQuality),
}

pub fn parse_mode(raw: Option<&str>) -> UpscaleMode {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return UpscaleMode::Auto;
    };
    match s.to_ascii_lowercase().as_str() {
        "cas" | "fsr" | "spatial" => UpscaleMode::Cas,
        "dlss" | "dlss-quality" | "quality" => UpscaleMode::Dlss(DlssQuality::Quality),
        "dlss-balanced" | "balanced" => UpscaleMode::Dlss(DlssQuality::Balanced),
        "dlss-performance" | "performance" => UpscaleMode::Dlss(DlssQuality::Performance),
        "dlss-ultra-performance" | "ultra-performance" => {
            UpscaleMode::Dlss(DlssQuality::UltraPerformance)
        }
        "dlss-dlaa" | "dlaa" => UpscaleMode::Dlss(DlssQuality::Dlaa),
        _ => UpscaleMode::Auto,
    }
}

pub fn parse_resolution(raw: Option<&str>) -> Option<(u32, u32)> {
    let s = raw?.trim();
    let (w, h) = s.split_once('x').or_else(|| s.split_once('X'))?;
    let w: u32 = w.trim().parse().ok()?;
    let h: u32 = h.trim().parse().ok()?;
    ((64..=16384).contains(&w) && (64..=16384).contains(&h)).then_some((w, h))
}

/// Radical-inverse Halton sequence in `(base 2, base 3)`, centered in `[-0.5, 0.5]`.
pub fn halton_jitter(frame_index: u64, phase_count: u32) -> (f32, f32) {
    let idx = (frame_index % u64::from(phase_count.max(1))) as u32 + 1;
    let radical = |mut n: u32, base: u32| -> f32 {
        let mut val = 0.0f32;
        let inv = 1.0f32 / (base as f32);
        let mut factor = inv;
        while n > 0 {
            val += (n % base) as f32 * factor;
            n /= base;
            factor *= inv;
        }
        val - 0.5
    };
    (radical(idx, 2), radical(idx, 3))
}

pub fn configured_mode() -> UpscaleMode {
    static MODE: OnceLock<UpscaleMode> = OnceLock::new();
    *MODE.get_or_init(|| {
        let raw = std::env::var("VICTORY_HOST_UPSCALER")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("VICTORY_UPSCALER").ok());
        parse_mode(raw.as_deref())
    })
}

pub fn configured_sharpness() -> f32 {
    static SHARP: OnceLock<f32> = OnceLock::new();
    *SHARP.get_or_init(|| {
        std::env::var("VICTORY_SHARPNESS")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .map_or(0.55, |s| s.clamp(0.0, 1.0))
    })
}

#[cfg(windows)]
fn configured_output_res() -> Option<(u32, u32)> {
    static OUT: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *OUT.get_or_init(|| parse_resolution(std::env::var("VICTORY_OUTPUT_RES").ok().as_deref()))
}

#[cfg(windows)]
pub(crate) use imp::{Upscaler, target_size, wants_upscale_pass};

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::ffi::{c_char, c_int, c_uint, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
    use windows::core::{Interface, s};

    use super::{
        DlssQuality, UpscaleMode, configured_mode, configured_output_res, configured_sharpness,
        halton_jitter,
    };
    use crate::host::{Host, transition};

    const SHADER_SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=8, b0), DescriptorTable(SRV(t0)), StaticSampler(s0, filter=FILTER_MIN_MAG_LINEAR_MIP_POINT, addressU=TEXTURE_ADDRESS_CLAMP, addressV=TEXTURE_ADDRESS_CLAMP, addressW=TEXTURE_ADDRESS_CLAMP)"

cbuffer Params : register(b0) {
    float2 inv_src;
    float2 inv_dst;
    float2 src_size;
    float sharpness;
    uint mode_flags;
};

float3 linear_to_srgb(float3 c) {
    c = max(c, float3(0.0, 0.0, 0.0));
    float3 lo = c * 12.92;
    float3 hi = 1.055 * pow(c, 1.0 / 2.4) - 0.055;
    return (c <= 0.0031308) ? lo : hi;
}

Texture2D<float4> SrcTex : register(t0);
SamplerState LinearClamp : register(s0);

struct VsOut {
    float4 pos : SV_Position;
    float2 uv  : TEXCOORD0;
};

[RootSignature(RS)]
VsOut vs_main(uint vid : SV_VertexID) {
    VsOut o;
    float2 uv = float2((vid << 1) & 2, vid & 2);
    o.pos = float4(uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    o.uv = uv;
    return o;
}

float3 sample_catmull_rom(float2 uv) {
    float2 pos = uv * src_size;
    float2 tc1 = floor(pos - 0.5) + 0.5;
    float2 f = pos - tc1;
    float2 f2 = f * f;
    float2 f3 = f2 * f;

    float2 w0 = f2 - 0.5 * (f3 + f);
    float2 w1 = 1.5 * f3 - 2.5 * f2 + 1.0;
    float2 w3 = 0.5 * (f3 - f2);
    float2 w2 = 1.0 - w0 - w1 - w3;

    float2 w12 = w1 + w2;
    float2 tc0 = (tc1 - 1.0) * inv_src;
    float2 tc3 = (tc1 + 2.0) * inv_src;
    float2 tc12 = (tc1 + w2 / max(w12, 1e-5)) * inv_src;

    float3 s0 = SrcTex.SampleLevel(LinearClamp, float2(tc12.x, tc0.y), 0.0).rgb * (w12.x * w0.y);
    float3 s1 = SrcTex.SampleLevel(LinearClamp, float2(tc0.x, tc12.y), 0.0).rgb * (w0.x * w12.y);
    float3 s2 = SrcTex.SampleLevel(LinearClamp, float2(tc12.x, tc12.y), 0.0).rgb * (w12.x * w12.y);
    float3 s3 = SrcTex.SampleLevel(LinearClamp, float2(tc3.x, tc12.y), 0.0).rgb * (w3.x * w12.y);
    float3 s4 = SrcTex.SampleLevel(LinearClamp, float2(tc12.x, tc3.y), 0.0).rgb * (w12.x * w3.y);
    float norm = (w12.x * w0.y) + (w0.x * w12.y) + (w12.x * w12.y) + (w3.x * w12.y) + (w12.x * w3.y);
    return max(float3(0.0, 0.0, 0.0), (s0 + s1 + s2 + s3 + s4) / max(norm, 1e-5));
}

[RootSignature(RS)]
float4 ps_main(VsOut i) : SV_Target0 {
    float3 base = sample_catmull_rom(i.uv);
    float4 center4 = SrcTex.SampleLevel(LinearClamp, i.uv, 0.0);
    float3 out_rgb = base;
    if (sharpness > 0.001) {
        float3 n = SrcTex.SampleLevel(LinearClamp, i.uv + float2(0.0, -inv_src.y), 0.0).rgb;
        float3 s = SrcTex.SampleLevel(LinearClamp, i.uv + float2(0.0,  inv_src.y), 0.0).rgb;
        float3 w = SrcTex.SampleLevel(LinearClamp, i.uv + float2(-inv_src.x, 0.0), 0.0).rgb;
        float3 e = SrcTex.SampleLevel(LinearClamp, i.uv + float2( inv_src.x, 0.0), 0.0).rgb;

        float3 mn = min(base, min(min(n, s), min(w, e)));
        float3 mx = max(base, max(max(n, s), max(w, e)));
        float3 peak = ((mode_flags & 1u) != 0u) ? max(float3(1.0, 1.0, 1.0), mx) : float3(1.0, 1.0, 1.0);
        float3 amp = saturate(min(mn, max(peak - mx, 0.0)) / max(mx, 1e-4));
        // Keep RCAS lobe strictly above -0.25 (FSR 1 RCAS max lobe -0.1875) so
        // `1.0 + 4.0 * lobe` never reaches 0.0 on flat surfaces (`0.0 / 0.0`).
        float3 lobe = -amp * (0.125 + 0.0625 * sharpness);
        float3 sharp = (base + lobe * (n + s + w + e)) / max(1.0 + 4.0 * lobe, 1e-4);
        out_rgb = clamp(sharp, mn, mx);
    }
    if ((mode_flags & 2u) != 0u) {
        out_rgb = linear_to_srgb(out_rgb);
    }
    return float4(out_rgb, center4.a);
}
"#;

    pub(crate) fn wants_upscale_pass(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> bool {
        (src_w, src_h) != (dst_w, dst_h) || !matches!(configured_mode(), UpscaleMode::Auto)
    }

    pub(crate) fn target_size(hwnd: HWND, src_w: u32, src_h: u32) -> (u32, u32) {
        if let Some((w, h)) = configured_output_res() {
            return (w, h);
        }
        let mut rect = RECT::default();
        // SAFETY: querying our presentation window's client bounds.
        if !hwnd.0.is_null() && unsafe { GetClientRect(hwnd, &mut rect) }.is_ok() {
            let cw = (rect.right - rect.left) as u32;
            let ch = (rect.bottom - rect.top) as u32;
            if cw >= 64 && ch >= 64 && (cw, ch) != (src_w, src_h) {
                return (cw, ch);
            }
        }
        if let UpscaleMode::Dlss(q) = configured_mode() {
            let (num, den) = q.scale_num_den();
            if num != den && src_w <= 2560 {
                return ((src_w * num / den).min(3840), (src_h * num / den).min(2160));
            }
        }
        (src_w, src_h)
    }

    // --- Dynamic NVIDIA NGX D3D12 FFI (no SDK binaries bundled) ---
    type FnInitProject = unsafe extern "C" fn(
        *const c_char,
        c_int,
        *const c_char,
        *const u16,
        *mut c_void,
        *const c_void,
        u32,
    ) -> u32;
    type FnAllocParams = unsafe extern "C" fn(*mut *mut c_void) -> u32;
    type FnDestroyParams = unsafe extern "C" fn(*mut c_void) -> u32;
    type FnCreateFeature =
        unsafe extern "C" fn(*mut c_void, c_int, *mut c_void, *mut *mut c_void) -> u32;
    type FnEvaluateFeature =
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const c_void) -> u32;
    type FnReleaseFeature = unsafe extern "C" fn(*mut c_void) -> u32;
    type FnSetUi = unsafe extern "C" fn(*mut c_void, *const c_char, c_uint);
    type FnSetI = unsafe extern "C" fn(*mut c_void, *const c_char, c_int);
    type FnSetF = unsafe extern "C" fn(*mut c_void, *const c_char, f32);
    type FnSetRes = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void);

    struct NgxApi {
        init_project: FnInitProject,
        alloc_params: FnAllocParams,
        destroy_params: FnDestroyParams,
        create_feature: FnCreateFeature,
        evaluate_feature: FnEvaluateFeature,
        release_feature: FnReleaseFeature,
        set_ui: FnSetUi,
        set_i: FnSetI,
        set_f: FnSetF,
        set_res: FnSetRes,
    }

    fn locate_ngx_dll() -> Option<PathBuf> {
        if let Ok(explicit) = std::env::var("VICTORY_NGX_DLL") {
            let p = PathBuf::from(explicit.trim());
            if p.is_file() {
                return Some(p);
            }
        }
        let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let repo = PathBuf::from(&sys_root).join(r"System32\DriverStore\FileRepository");
        if let Ok(entries) = std::fs::read_dir(repo) {
            let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(s) = name.to_str() else { continue };
                if !s.to_ascii_lowercase().starts_with("nv") {
                    continue;
                }
                let candidate = entry.path().join("_nvngx.dll");
                if candidate.is_file() {
                    let mtime = candidate
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                        best = Some((mtime, candidate));
                    }
                }
            }
            if let Some((_, path)) = best {
                return Some(path);
            }
        }
        let sys32 = PathBuf::from(sys_root).join(r"System32\_nvngx.dll");
        sys32.is_file().then_some(sys32)
    }

    fn ngx_api() -> Option<&'static NgxApi> {
        static API: OnceLock<Option<NgxApi>> = OnceLock::new();
        API.get_or_init(|| {
            use windows_sys::Win32::System::LibraryLoader::{
                GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
            };
            let path = locate_ngx_dll()?;
            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: loading NVIDIA's signed `_nvngx.dll` from DriverStore / user path.
            let module = unsafe {
                LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH)
            };
            if module.is_null() {
                return None;
            }
            let sym = |name: &[u8]| -> Option<usize> {
                // SAFETY: valid module and NUL-terminated symbol name.
                unsafe { GetProcAddress(module, name.as_ptr()) }.map(|f| f as usize)
            };
            // SAFETY: transmuting resolved NGX C ABI function pointers.
            unsafe {
                Some(NgxApi {
                    init_project: std::mem::transmute::<usize, FnInitProject>(sym(
                        b"NVSDK_NGX_D3D12_Init_with_ProjectID\0",
                    )?),
                    alloc_params: std::mem::transmute::<usize, FnAllocParams>(sym(
                        b"NVSDK_NGX_D3D12_AllocateParameters\0",
                    )?),
                    destroy_params: std::mem::transmute::<usize, FnDestroyParams>(sym(
                        b"NVSDK_NGX_D3D12_DestroyParameters\0",
                    )?),
                    create_feature: std::mem::transmute::<usize, FnCreateFeature>(sym(
                        b"NVSDK_NGX_D3D12_CreateFeature\0",
                    )?),
                    evaluate_feature: std::mem::transmute::<usize, FnEvaluateFeature>(sym(
                        b"NVSDK_NGX_D3D12_EvaluateFeature\0",
                    )?),
                    release_feature: std::mem::transmute::<usize, FnReleaseFeature>(sym(
                        b"NVSDK_NGX_D3D12_ReleaseFeature\0",
                    )?),
                    set_ui: std::mem::transmute::<usize, FnSetUi>(sym(
                        b"NVSDK_NGX_Parameter_SetUI\0",
                    )?),
                    set_i: std::mem::transmute::<usize, FnSetI>(sym(b"NVSDK_NGX_Parameter_SetI\0")?),
                    set_f: std::mem::transmute::<usize, FnSetF>(sym(b"NVSDK_NGX_Parameter_SetF\0")?),
                    set_res: std::mem::transmute::<usize, FnSetRes>(sym(
                        b"NVSDK_NGX_Parameter_SetD3d12Resource\0",
                    )?),
                })
            }
        })
        .as_ref()
    }

    struct NgxSession {
        params: *mut c_void,
        handle: *mut c_void,
        output: ID3D12Resource,
        key: (u32, u32, u32, u32, DXGI_FORMAT, bool, DlssQuality),
        frame_count: u64,
    }

    impl Drop for NgxSession {
        fn drop(&mut self) {
            if let Some(api) = ngx_api() {
                // SAFETY: releasing NGX feature handle and parameter block created by `api`.
                unsafe {
                    if !self.handle.is_null() {
                        let _ = (api.release_feature)(self.handle);
                    }
                    if !self.params.is_null() {
                        let _ = (api.destroy_params)(self.params);
                    }
                }
            }
        }
    }

    pub(crate) struct Upscaler {
        root: Option<ID3D12RootSignature>,
        vs_bytes: Vec<u8>,
        ps_bytes: Vec<u8>,
        psos: HashMap<i32, ID3D12PipelineState>,
        srv_heaps: Vec<ID3D12DescriptorHeap>,
        ngx_inited: Option<bool>,
        ngx: Option<NgxSession>,
    }

    // SAFETY: D3D12 COM interfaces and NGX handles are accessed strictly under `Presenter` mutex.
    unsafe impl Send for Upscaler {}
    // SAFETY: as above.
    unsafe impl Sync for Upscaler {}

    impl Upscaler {
        pub(crate) fn new(device: &ID3D12Device) -> windows::core::Result<Self> {
            let compile = |entry: windows::core::PCSTR, target: windows::core::PCSTR| {
                let mut code: Option<ID3DBlob> = None;
                let mut errs: Option<ID3DBlob> = None;
                // SAFETY: compiling static HLSL source with FXC.
                unsafe {
                    D3DCompile(
                        SHADER_SOURCE.as_ptr().cast(),
                        SHADER_SOURCE.len(),
                        s!("upscale"),
                        None,
                        None,
                        entry,
                        target,
                        0,
                        0,
                        &mut code,
                        Some(&mut errs),
                    )?;
                    let blob = code.ok_or_else(windows::core::Error::empty)?;
                    Ok::<Vec<u8>, windows::core::Error>(
                        std::slice::from_raw_parts(
                            blob.GetBufferPointer().cast::<u8>(),
                            blob.GetBufferSize(),
                        )
                        .to_vec(),
                    )
                }
            };
            let vs_bytes = compile(s!("vs_main"), s!("vs_5_1"))?;
            let ps_bytes = compile(s!("ps_main"), s!("ps_5_1"))?;
            // SAFETY: embedded root signature in `vs_bytes`.
            unsafe {
                let root: ID3D12RootSignature = device.CreateRootSignature(0, &vs_bytes)?;
                Ok(Self {
                    root: Some(root),
                    vs_bytes,
                    ps_bytes,
                    psos: HashMap::new(),
                    srv_heaps: Vec::new(),
                    ngx_inited: None,
                    ngx: None,
                })
            }
        }

        /// Records an upscale pass from `src` (restored to `src_state`) into
        /// `dst_back`, leaving `dst_back` in `D3D12_RESOURCE_STATE_COPY_DEST`
        /// (matching `Composer::record` so capture, HUD panel, and final
        /// `PRESENT` transition follow the same path).
        #[allow(clippy::too_many_arguments)]
        pub(crate) fn run(
            &mut self,
            host: &Host,
            slot: usize,
            list: &ID3D12GraphicsCommandList,
            src: &ID3D12Resource,
            src_state: D3D12_RESOURCE_STATES,
            dst_back: &ID3D12Resource,
            dst_format: DXGI_FORMAT,
            src_w: u32,
            src_h: u32,
            dst_w: u32,
            dst_h: u32,
            hdr: bool,
        ) -> bool {
            // SAFETY: plain descriptor query on a live resource.
            if (unsafe { src.GetDesc() }.Flags & D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE)
                != D3D12_RESOURCE_FLAG_NONE
            {
                return false;
            }
            if let UpscaleMode::Dlss(quality) = configured_mode()
                && slot == 0
                && self.try_ngx_dlss(
                    &host.device, list, src, src_state, dst_back, dst_format, src_w, src_h, dst_w,
                    dst_h, hdr, quality,
                )
            {
                return true;
            }
            self.run_spatial(
                host, slot, list, src, src_state, dst_back, dst_format, src_w, src_h, dst_w, dst_h,
                hdr,
            )
        }

        #[allow(clippy::too_many_arguments)]
        fn try_ngx_dlss(
            &mut self,
            device: &ID3D12Device,
            list: &ID3D12GraphicsCommandList,
            src: &ID3D12Resource,
            src_state: D3D12_RESOURCE_STATES,
            dst_back: &ID3D12Resource,
            dst_format: DXGI_FORMAT,
            src_w: u32,
            src_h: u32,
            dst_w: u32,
            dst_h: u32,
            hdr: bool,
            quality: DlssQuality,
        ) -> bool {
            const NGX_SUCCESS: u32 = 1;
            const FEATURE_SUPERSAMPLING: c_int = 1;
            let Some(api) = ngx_api() else {
                if self.ngx_inited.is_none() {
                    self.ngx_inited = Some(false);
                    tracing::info!("DLSS NGX runtime not present on host; using D3D12 bicubic+CAS upscaler");
                }
                return false;
            };
            if self.ngx_inited.is_none() {
                let data_dir: Vec<u16> = std::env::temp_dir()
                    .as_os_str()
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();
                // SAFETY: initializing NGX D3D12 runtime once with our `ID3D12Device`.
                let status = unsafe {
                    (api.init_project)(
                        c"a0f57b54-1daf-4934-90ae-c4035c19df04".as_ptr(),
                        0,
                        c"0.1.0".as_ptr(),
                        data_dir.as_ptr(),
                        device.as_raw(),
                        std::ptr::null(),
                        0x0000_0015,
                    )
                };
                let ok = status == NGX_SUCCESS;
                self.ngx_inited = Some(ok);
                if !ok {
                    tracing::info!(
                        status = format_args!("{status:#x}"),
                        "NGX D3D12 init declined; falling back to D3D12 bicubic+CAS upscaler"
                    );
                    return false;
                }
                tracing::info!(?quality, "NVIDIA NGX DLSS Super Resolution initialized");
            }
            if self.ngx_inited != Some(true) {
                return false;
            }
            let key = (src_w, src_h, dst_w, dst_h, dst_format, hdr, quality);
            let recreate = self.ngx.as_ref().is_none_or(|s| s.key != key);
            if recreate {
                self.ngx = None;
                let heap = D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                };
                let uav_desc = D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Alignment: 0,
                    Width: u64::from(dst_w),
                    Height: dst_h,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: dst_format,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                    Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                };
                let mut uav_tex: Option<ID3D12Resource> = None;
                // SAFETY: creating intermediate UAV output texture and NGX DLSS feature.
                unsafe {
                    if device
                        .CreateCommittedResource(
                            &heap,
                            D3D12_HEAP_FLAG_NONE,
                            &uav_desc,
                            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                            None,
                            &mut uav_tex,
                        )
                        .is_err()
                    {
                        return false;
                    }
                    let Some(output) = uav_tex else { return false };
                    let mut params = std::ptr::null_mut();
                    if (api.alloc_params)(&mut params) != NGX_SUCCESS || params.is_null() {
                        return false;
                    }
                    let create_flags = (if hdr { 1 << 1 } else { 0 }) | (1 << 3); // IsHDR | AutoExposure
                    (api.set_ui)(params, c"Width".as_ptr(), src_w);
                    (api.set_ui)(params, c"Height".as_ptr(), src_h);
                    (api.set_ui)(params, c"OutWidth".as_ptr(), dst_w);
                    (api.set_ui)(params, c"OutHeight".as_ptr(), dst_h);
                    (api.set_i)(params, c"PerfQualityValue".as_ptr(), quality.ngx_code());
                    (api.set_i)(
                        params,
                        c"DLSS.Feature.Create.Flags".as_ptr(),
                        create_flags,
                    );
                    let mut handle = std::ptr::null_mut();
                    let status = (api.create_feature)(
                        list.as_raw(),
                        FEATURE_SUPERSAMPLING,
                        params,
                        &mut handle,
                    );
                    if status != NGX_SUCCESS || handle.is_null() {
                        let _ = (api.destroy_params)(params);
                        return false;
                    }
                    self.ngx = Some(NgxSession {
                        params,
                        handle,
                        output,
                        key,
                        frame_count: 0,
                    });
                }
            }
            let Some(session) = self.ngx.as_mut() else {
                return false;
            };
            let phase_count = (8 * (dst_w.max(src_w) / src_w.max(1)).pow(2)).clamp(8, 64);
            let (jx, jy) = halton_jitter(session.frame_count, phase_count);
            let reset = i32::from(session.frame_count == 0);
            session.frame_count = session.frame_count.wrapping_add(1);

            // SAFETY: transitioning resources, evaluating DLSS on `list`, and copying to `dst_back`.
            unsafe {
                if src_state != D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE {
                    list.ResourceBarrier(&[transition(
                        src,
                        src_state,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    )]);
                }
                (api.set_res)(session.params, c"Color".as_ptr(), src.as_raw());
                (api.set_res)(
                    session.params,
                    c"Output".as_ptr(),
                    session.output.as_raw(),
                );
                (api.set_f)(session.params, c"Jitter.Offset.X".as_ptr(), jx);
                (api.set_f)(session.params, c"Jitter.Offset.Y".as_ptr(), jy);
                (api.set_f)(
                    session.params,
                    c"Sharpness".as_ptr(),
                    configured_sharpness(),
                );
                (api.set_i)(session.params, c"Reset".as_ptr(), reset);
                (api.set_ui)(
                    session.params,
                    c"DLSS.Render.Subrect.Dimensions.Width".as_ptr(),
                    src_w,
                );
                (api.set_ui)(
                    session.params,
                    c"DLSS.Render.Subrect.Dimensions.Height".as_ptr(),
                    src_h,
                );
                let eval = (api.evaluate_feature)(
                    list.as_raw(),
                    session.handle,
                    session.params,
                    std::ptr::null(),
                );
                if eval != NGX_SUCCESS {
                    if src_state != D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE {
                        list.ResourceBarrier(&[transition(
                            src,
                            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                            src_state,
                        )]);
                    }
                    return false;
                }
                list.ResourceBarrier(&[
                    transition(
                        &session.output,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    ),
                    transition(
                        dst_back,
                        D3D12_RESOURCE_STATE_PRESENT,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                    ),
                ]);
                list.CopyResource(dst_back, &session.output);
                if src_state != D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE {
                    list.ResourceBarrier(&[
                        transition(
                            &session.output,
                            D3D12_RESOURCE_STATE_COPY_SOURCE,
                            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                        ),
                        transition(
                            src,
                            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                            src_state,
                        ),
                    ]);
                } else {
                    list.ResourceBarrier(&[transition(
                        &session.output,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    )]);
                }
                true
            }
        }

        #[allow(clippy::too_many_arguments)]
        fn run_spatial(
            &mut self,
            host: &Host,
            slot: usize,
            list: &ID3D12GraphicsCommandList,
            src: &ID3D12Resource,
            src_state: D3D12_RESOURCE_STATES,
            dst_back: &ID3D12Resource,
            dst_format: DXGI_FORMAT,
            src_w: u32,
            src_h: u32,
            dst_w: u32,
            dst_h: u32,
            hdr: bool,
        ) -> bool {
            let Some(root) = &self.root else { return false };
            let device = &host.device;
            // SAFETY: plain descriptor query on a live source texture.
            let src_desc = unsafe { src.GetDesc() };
            let (srv_format, src_srgb) = match src_desc.Format {
                DXGI_FORMAT_R8G8B8A8_TYPELESS | DXGI_FORMAT_R8G8B8A8_UNORM => {
                    (DXGI_FORMAT_R8G8B8A8_UNORM, false)
                }
                DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => (DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, true),
                DXGI_FORMAT_B8G8R8A8_TYPELESS | DXGI_FORMAT_B8G8R8A8_UNORM => {
                    (DXGI_FORMAT_B8G8R8A8_UNORM, false)
                }
                DXGI_FORMAT_B8G8R8A8_UNORM_SRGB => (DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, true),
                DXGI_FORMAT_R10G10B10A2_TYPELESS | DXGI_FORMAT_R10G10B10A2_UNORM => {
                    (DXGI_FORMAT_R10G10B10A2_UNORM, false)
                }
                DXGI_FORMAT_R16G16B16A16_TYPELESS
                | DXGI_FORMAT_R16G16B16A16_FLOAT
                | DXGI_FORMAT_R16G16B16A16_UNORM => (
                    if src_desc.Format == DXGI_FORMAT_R16G16B16A16_UNORM {
                        DXGI_FORMAT_R16G16B16A16_UNORM
                    } else {
                        DXGI_FORMAT_R16G16B16A16_FLOAT
                    },
                    false,
                ),
                _ => return false,
            };
            if !self.psos.contains_key(&dst_format.0) {
                let mut rtv_formats = [DXGI_FORMAT_UNKNOWN; 8];
                rtv_formats[0] = dst_format;
                let target_blend = D3D12_RENDER_TARGET_BLEND_DESC {
                    RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
                    ..Default::default()
                };
                // SAFETY: creating fullscreen upscaler PSO for `dst_format`.
                let made: windows::core::Result<ID3D12PipelineState> = unsafe {
                    device.CreateGraphicsPipelineState(&D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                        pRootSignature: std::mem::transmute_copy(root),
                        VS: D3D12_SHADER_BYTECODE {
                            pShaderBytecode: self.vs_bytes.as_ptr().cast(),
                            BytecodeLength: self.vs_bytes.len(),
                        },
                        PS: D3D12_SHADER_BYTECODE {
                            pShaderBytecode: self.ps_bytes.as_ptr().cast(),
                            BytecodeLength: self.ps_bytes.len(),
                        },
                        BlendState: D3D12_BLEND_DESC {
                            RenderTarget: [target_blend; 8],
                            ..Default::default()
                        },
                        SampleMask: u32::MAX,
                        RasterizerState: D3D12_RASTERIZER_DESC {
                            FillMode: D3D12_FILL_MODE_SOLID,
                            CullMode: D3D12_CULL_MODE_NONE,
                            DepthClipEnable: true.into(),
                            ..Default::default()
                        },
                        DepthStencilState: D3D12_DEPTH_STENCIL_DESC::default(),
                        PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                        NumRenderTargets: 1,
                        RTVFormats: rtv_formats,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        ..Default::default()
                    })
                };
                match made {
                    Ok(pso) => {
                        self.psos.insert(dst_format.0, pso);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, format = dst_format.0, "upscaler PSO failed");
                        return false;
                    }
                }
            }
            while self.srv_heaps.len() <= slot {
                // SAFETY: creating a per-slot 1-descriptor shader-visible SRV heap.
                let Ok(heap) = (unsafe {
                    device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                        Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                        NumDescriptors: 1,
                        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                        NodeMask: 0,
                    })
                }) else {
                    return false;
                };
                self.srv_heaps.push(heap);
            }
            let srv_heap = &self.srv_heaps[slot];
            let pso = &self.psos[&dst_format.0];
            // SAFETY: recording fullscreen Catmull-Rom + CAS upscale pass onto `list`.
            unsafe {
                let srv_desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: srv_format,
                    ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2D: D3D12_TEX2D_SRV {
                            MostDetailedMip: 0,
                            MipLevels: 1,
                            PlaneSlice: 0,
                            ResourceMinLODClamp: 0.0,
                        },
                    },
                };
                let srv_cpu = srv_heap.GetCPUDescriptorHandleForHeapStart();
                let srv_gpu = srv_heap.GetGPUDescriptorHandleForHeapStart();
                device.CreateShaderResourceView(Some(src), Some(&srv_desc), srv_cpu);

                let rtv_cpu = host.rtv(Some(dst_back), None);

                if src_state != D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE {
                    list.ResourceBarrier(&[
                        transition(src, src_state, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE),
                        transition(
                            dst_back,
                            D3D12_RESOURCE_STATE_PRESENT,
                            D3D12_RESOURCE_STATE_RENDER_TARGET,
                        ),
                    ]);
                } else {
                    list.ResourceBarrier(&[transition(
                        dst_back,
                        D3D12_RESOURCE_STATE_PRESENT,
                        D3D12_RESOURCE_STATE_RENDER_TARGET,
                    )]);
                }

                let heaps = [Some(srv_heap.clone())];
                list.SetDescriptorHeaps(&heaps);
                list.SetGraphicsRootSignature(root);
                list.SetPipelineState(pso);
                let viewport = D3D12_VIEWPORT {
                    TopLeftX: 0.0,
                    TopLeftY: 0.0,
                    Width: dst_w as f32,
                    Height: dst_h as f32,
                    MinDepth: 0.0,
                    MaxDepth: 1.0,
                };
                let scissor = RECT {
                    left: 0,
                    top: 0,
                    right: dst_w as i32,
                    bottom: dst_h as i32,
                };
                list.RSSetViewports(&[viewport]);
                list.RSSetScissorRects(&[scissor]);
                list.OMSetRenderTargets(1, Some(&rtv_cpu), false, None);

                let mode_flags = u32::from(hdr) | (u32::from(src_srgb) << 1);
                let constants: [u32; 8] = [
                    (1.0f32 / (src_w.max(1) as f32)).to_bits(),
                    (1.0f32 / (src_h.max(1) as f32)).to_bits(),
                    (1.0f32 / (dst_w.max(1) as f32)).to_bits(),
                    (1.0f32 / (dst_h.max(1) as f32)).to_bits(),
                    (src_w as f32).to_bits(),
                    (src_h as f32).to_bits(),
                    configured_sharpness().to_bits(),
                    mode_flags,
                ];
                list.SetGraphicsRoot32BitConstants(0, 8, constants.as_ptr().cast(), 0);
                list.SetGraphicsRootDescriptorTable(1, srv_gpu);
                list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                list.DrawInstanced(3, 1, 0, 0);

                if src_state != D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE {
                    list.ResourceBarrier(&[
                        transition(src, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE, src_state),
                        transition(
                            dst_back,
                            D3D12_RESOURCE_STATE_RENDER_TARGET,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                        ),
                    ]);
                } else {
                    list.ResourceBarrier(&[transition(
                        dst_back,
                        D3D12_RESOURCE_STATE_RENDER_TARGET,
                        D3D12_RESOURCE_STATE_COPY_DEST,
                    )]);
                }
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dlss_and_cas_modes_and_resolutions() {
        assert_eq!(parse_mode(None), UpscaleMode::Auto);
        assert_eq!(
            parse_mode(Some("dlss")),
            UpscaleMode::Dlss(DlssQuality::Quality)
        );
        assert_eq!(
            parse_mode(Some("dlss-performance")),
            UpscaleMode::Dlss(DlssQuality::Performance)
        );
        assert_eq!(
            parse_mode(Some("dlss-balanced")),
            UpscaleMode::Dlss(DlssQuality::Balanced)
        );
        assert_eq!(
            parse_mode(Some("dlss-dlaa")),
            UpscaleMode::Dlss(DlssQuality::Dlaa)
        );
        assert_eq!(parse_mode(Some("cas")), UpscaleMode::Cas);
        assert_eq!(parse_resolution(Some("3840x2160")), Some((3840, 2160)));
        assert_eq!(parse_resolution(Some("2560X1440")), Some((2560, 1440)));
        assert_eq!(parse_resolution(Some("invalid")), None);
    }

    #[test]
    fn halton_jitter_stays_centered_within_half_pixel() {
        for f in 0..32 {
            let (jx, jy) = halton_jitter(f, 16);
            assert!((-0.5..=0.5).contains(&jx));
            assert!((-0.5..=0.5).contains(&jy));
        }
        assert_eq!(halton_jitter(0, 16), halton_jitter(16, 16));
    }
}
