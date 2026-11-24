//! Accessibility: speech synthesis for the title's narration (`XSpeechSynthesizer`).
//! On Windows, text and SSML streams are rendered to 16 kHz mono 16-bit RIFF
//! WAVE audio through SAPI 5 (`ISpVoice` + `ISpCustomStream`), falling back
//! cleanly to a valid zero-sample WAVE stream when no host voice is available.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::collections::BTreeMap;
use std::ffi::{c_char, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::xasync::{E_INVALIDARG, E_NOT_SUFFICIENT_BUFFER};
use super::xgameruntime::{
    Guid, HRESULT, Interface, Methods, Object, S_OK, add_ref, missing, put, text,
};

/// Closed captions, high contrast, speech to text and synthesis.
pub const PROVIDER: Guid = Guid::new(
    0x3e24_1977,
    0x6237,
    0x41e9,
    [0x85, 0x59, 0x10, 0x2c, 0x2d, 0x99, 0x83, 0xf1],
);
/// Unchanged since 2020.
pub const IID: Guid = Guid::new(
    0xd722_b373,
    0x8c4d,
    0x4692,
    [0x9e, 0x51, 0xd6, 0xad, 0x9b, 0x37, 0xaa, 0x7d],
);

pub static OBJECT: Object = Object::new("accessibility", &VTABLE);
pub const INTERFACES: &[Interface] = &[(PROVIDER, IID, &OBJECT)];

/// A WAVE file of 16 kHz mono 16-bit PCM with no samples: RIFF size 36,
/// a 16-byte format chunk (PCM, 1 channel, 16,000 Hz, 32,000 bytes/s,
/// block 2, 16 bits), an empty data chunk.
const SILENCE: [u8; 44] =
    *b"RIFF\x24\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data\0\0\0\0";

#[repr(C)]
pub struct VoiceInformation {
    pub description: *const c_char,
    pub display_name: *const c_char,
    pub gender: u32,
    pub voice_id: *const c_char,
    pub language: *const c_char,
}

static NEXT_HANDLE: AtomicUsize = AtomicUsize::new(1);
static SYNTHS: Mutex<BTreeMap<usize, Option<String>>> = Mutex::new(BTreeMap::new());
static STREAMS: Mutex<BTreeMap<usize, Vec<u8>>> = Mutex::new(BTreeMap::new());

fn new_handle() -> usize {
    NEXT_HANDLE.fetch_add(1, Ordering::Relaxed)
}

fn pack_wav_16khz_mono(pcm: &[u8]) -> Vec<u8> {
    let data_len = (pcm.len() & !1).min((u32::MAX - 36) as usize);
    let riff_len = 36u32 + data_len as u32;
    let mut wav = Vec::with_capacity(44 + data_len);
    wav.extend_from_slice(&SILENCE);
    wav[4..8].copy_from_slice(&riff_len.to_le_bytes());
    wav[40..44].copy_from_slice(&(data_len as u32).to_le_bytes());
    wav.extend_from_slice(&pcm[..data_len]);
    wav
}

fn strip_ssml_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

#[cfg(windows)]
fn synthesize_wav(words: &str, ssml: bool) -> Vec<u8> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    #[repr(C, packed(2))]
    struct WaveFormatEx {
        format_tag: u16,
        channels: u16,
        samples_per_sec: u32,
        avg_bytes_per_sec: u32,
        block_align: u16,
        bits_per_sample: u16,
        cb_size: u16,
    }

    const CLSID_SP_VOICE: Guid = Guid::new(
        0x9674_9377,
        0x3391,
        0x11d2,
        [0x9e, 0xe3, 0x00, 0xc0, 0x4f, 0x79, 0x73, 0x96],
    );
    const IID_ISP_VOICE: Guid = Guid::new(
        0x6c44_df74,
        0x72b9,
        0x4992,
        [0xa1, 0xec, 0xef, 0x99, 0x6e, 0x04, 0x22, 0xd4],
    );
    const CLSID_SP_STREAM: Guid = Guid::new(
        0x715d_9c59,
        0x4442,
        0x11d2,
        [0x96, 0x05, 0x00, 0xc0, 0x4f, 0x8e, 0xe6, 0x28],
    );
    const IID_ISP_STREAM: Guid = Guid::new(
        0x12e3_cca9,
        0x7518,
        0x44c5,
        [0xa5, 0xe7, 0xba, 0x5a, 0x79, 0xcb, 0x92, 0x9e],
    );
    const SPDFID_WAVE_FORMAT_EX: Guid = Guid::new(
        0xc31a_dbae,
        0x527f,
        0x4ff5,
        [0xa2, 0x30, 0xf6, 0x2b, 0xb6, 0x1f, 0xf7, 0x0c],
    );

    let trimmed = words.trim();
    if trimmed.is_empty() {
        return SILENCE.to_vec();
    }

    type CoInitEx = unsafe extern "system" fn(*const c_void, u32) -> i32;
    type CoUninit = unsafe extern "system" fn();
    type CoCreate = unsafe extern "system" fn(
        *const Guid,
        *mut c_void,
        u32,
        *const Guid,
        *mut *mut c_void,
    ) -> i32;
    type CreateMemStream = unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> i32;
    type Release = unsafe extern "system" fn(*mut c_void) -> u32;
    type SeqRead = unsafe extern "system" fn(*mut c_void, *mut u8, u32, *mut u32) -> i32;
    type StreamSeek = unsafe extern "system" fn(*mut c_void, i64, u32, *mut u64) -> i32;
    type SetBase = unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const Guid,
        *const WaveFormatEx,
    ) -> i32;
    type SetOutput = unsafe extern "system" fn(*mut c_void, *mut c_void, i32) -> i32;
    type Speak = unsafe extern "system" fn(*mut c_void, *const u16, u32, *mut u32) -> i32;

    let dll: Vec<u16> = "ole32.dll\0".encode_utf16().collect();
    // SAFETY: dynamic resolution of ole32 COM entry points and SAPI 5 vtable calls.
    unsafe {
        let lib = LoadLibraryW(dll.as_ptr());
        if lib.is_null() {
            return SILENCE.to_vec();
        }
        let Some(co_init) = GetProcAddress(lib, c"CoInitializeEx".as_ptr().cast()) else {
            return SILENCE.to_vec();
        };
        let Some(co_uninit) = GetProcAddress(lib, c"CoUninitialize".as_ptr().cast()) else {
            return SILENCE.to_vec();
        };
        let Some(co_create) = GetProcAddress(lib, c"CoCreateInstance".as_ptr().cast()) else {
            return SILENCE.to_vec();
        };
        let Some(mem_stream) = GetProcAddress(lib, c"CreateStreamOnHGlobal".as_ptr().cast())
        else {
            return SILENCE.to_vec();
        };
        let co_init: CoInitEx = std::mem::transmute(co_init);
        let co_uninit: CoUninit = std::mem::transmute(co_uninit);
        let co_create: CoCreate = std::mem::transmute(co_create);
        let mem_stream: CreateMemStream = std::mem::transmute(mem_stream);

        let init_hr = co_init(std::ptr::null(), 0); // COINIT_MULTITHREADED
        let uninit = init_hr == 0 || init_hr == 1;

        let vtbl = |obj: *mut c_void, idx: usize| -> *const () {
            *(*(obj as *const *const *const ())).add(idx)
        };
        let release_obj = |obj: *mut c_void| {
            if !obj.is_null() {
                let rel: Release = std::mem::transmute(vtbl(obj, 2));
                rel(obj);
            }
        };

        let mut base = std::ptr::null_mut();
        let mut custom = std::ptr::null_mut();
        let mut voice = std::ptr::null_mut();
        let mut pcm = Vec::new();

        const CLSCTX_INPROC_SERVER: u32 = 0x1;
        if mem_stream(std::ptr::null_mut(), 1, &mut base) >= 0
            && co_create(
                &CLSID_SP_STREAM,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ISP_STREAM,
                &mut custom,
            ) >= 0
            && co_create(
                &CLSID_SP_VOICE,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ISP_VOICE,
                &mut voice,
            ) >= 0
        {
            let wfx = WaveFormatEx {
                format_tag: 1,
                channels: 1,
                samples_per_sec: 16_000,
                avg_bytes_per_sec: 32_000,
                block_align: 2,
                bits_per_sample: 16,
                cb_size: 0,
            };
            let set_base: SetBase = std::mem::transmute(vtbl(custom, 15));
            let set_out: SetOutput = std::mem::transmute(vtbl(voice, 13));
            let speak: Speak = std::mem::transmute(vtbl(voice, 20));
            if set_base(custom, base, &SPDFID_WAVE_FORMAT_EX, &wfx) >= 0
                && set_out(voice, custom, 0) >= 0
            {
                let wide: Vec<u16> = trimmed.encode_utf16().chain([0]).collect();
                // SPF_IS_XML = 0x8, SPF_IS_NOT_XML = 0x10.
                let flags = if ssml { 0x8 } else { 0x10 };
                let mut spoken = speak(voice, wide.as_ptr(), flags, std::ptr::null_mut()) >= 0;
                if !spoken && ssml {
                    let plain = strip_ssml_tags(trimmed);
                    let plain_w: Vec<u16> = plain.trim().encode_utf16().chain([0]).collect();
                    spoken = speak(voice, plain_w.as_ptr(), 0x10, std::ptr::null_mut()) >= 0;
                }
                if spoken {
                    let seek: StreamSeek = std::mem::transmute(vtbl(base, 5));
                    let read: SeqRead = std::mem::transmute(vtbl(base, 3));
                    let mut end = 0u64;
                    if seek(base, 0, 2, &mut end) >= 0
                        && end > 0
                        && end <= (16 << 20)
                        && seek(base, 0, 0, std::ptr::null_mut()) >= 0
                    {
                        pcm.resize(end as usize, 0);
                        let mut got = 0u32;
                        if read(base, pcm.as_mut_ptr(), end as u32, &mut got) >= 0 {
                            pcm.truncate(got as usize);
                        } else {
                            pcm.clear();
                        }
                    }
                }
            }
        }
        release_obj(voice);
        release_obj(custom);
        release_obj(base);
        if uninit {
            co_uninit();
        }
        if pcm.is_empty() {
            SILENCE.to_vec()
        } else {
            pack_wav_16khz_mono(&pcm)
        }
    }
}

#[cfg(not(windows))]
fn synthesize_wav(_words: &str, _ssml: bool) -> Vec<u8> {
    SILENCE.to_vec()
}

type This = *const Object;

extern "system" fn enumerate_voices(
    _: This,
    context: *mut c_void,
    callback: *const c_void,
) -> HRESULT {
    if !callback.is_null() {
        type Callback = extern "system" fn(*const VoiceInformation, *mut c_void) -> bool;
        // SAFETY: the caller passes a valid voice enumeration callback.
        let cb: Callback = unsafe { std::mem::transmute(callback) };
        let info = VoiceInformation {
            description: c"System Narrator Voice".as_ptr(),
            display_name: c"Default".as_ptr(),
            gender: 0,
            voice_id: c"default".as_ptr(),
            language: c"en-US".as_ptr(),
        };
        let _ = cb(&info, context);
    }
    S_OK
}

extern "system" fn create(_: This, out: *mut *const u8) -> HRESULT {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let id = new_handle();
    SYNTHS.lock().unwrap_or_else(|e| e.into_inner()).insert(id, None);
    put(out, id as *const u8)
}

extern "system" fn close_synth(_: This, synth: *const u8) -> HRESULT {
    if synth.is_null() {
        return E_INVALIDARG;
    }
    SYNTHS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(synth as usize));
    S_OK
}

extern "system" fn set_default_voice(_: This, synth: *const u8) -> HRESULT {
    if synth.is_null() {
        return E_INVALIDARG;
    }
    if let Some(slot) = SYNTHS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&(synth as usize))
    {
        *slot = None;
    }
    S_OK
}

extern "system" fn set_custom_voice(_: This, synth: *const u8, voice: *const c_char) -> HRESULT {
    if synth.is_null() {
        return E_INVALIDARG;
    }
    let chosen = text(voice).map(str::to_owned);
    if let Some(slot) = SYNTHS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&(synth as usize))
    {
        *slot = chosen;
    }
    S_OK
}

fn make_stream(synth: *const u8, words: *const c_char, ssml: bool, out: *mut *const u8) -> HRESULT {
    if synth.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let Some(voice) = SYNTHS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(synth as usize))
        .cloned()
    else {
        return E_INVALIDARG;
    };
    let content = text(words).unwrap_or_default();
    let wav = synthesize_wav(content, ssml);
    tracing::debug!(
        ssml,
        voice = voice.as_deref(),
        bytes = wav.len(),
        text = content,
        "speech stream synthesized"
    );
    let id = new_handle();
    STREAMS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, wav);
    put(out, id as *const u8)
}

extern "system" fn create_stream(
    _: This,
    synth: *const u8,
    words: *const c_char,
    out: *mut *const u8,
) -> HRESULT {
    make_stream(synth, words, false, out)
}

extern "system" fn create_ssml_stream(
    _: This,
    synth: *const u8,
    ssml: *const c_char,
    out: *mut *const u8,
) -> HRESULT {
    make_stream(synth, ssml, true, out)
}

extern "system" fn close_stream(_: This, stream: *const u8) -> HRESULT {
    if stream.is_null() {
        return E_INVALIDARG;
    }
    STREAMS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(stream as usize));
    S_OK
}

extern "system" fn stream_size(_: This, stream: *const u8, out: *mut usize) -> HRESULT {
    if stream.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let Some(len) = STREAMS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(stream as usize))
        .map(Vec::len)
    else {
        return E_INVALIDARG;
    };
    put(out, len)
}

extern "system" fn stream_data(
    _: This,
    stream: *const u8,
    size: usize,
    buffer: *mut u8,
    used: *mut usize,
) -> HRESULT {
    if stream.is_null() || buffer.is_null() {
        return E_NOT_SUFFICIENT_BUFFER;
    }
    let streams = STREAMS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(data) = streams.get(&(stream as usize)).map(Vec::as_slice) else {
        return E_INVALIDARG;
    };
    if size < data.len() {
        return E_NOT_SUFFICIENT_BUFFER;
    }
    // SAFETY: the buffer holds `size >= data.len()` bytes, checked above.
    unsafe { buffer.copy_from_nonoverlapping(data.as_ptr(), data.len()) };
    if !used.is_null() {
        // SAFETY: checked non-null.
        unsafe { used.write(data.len()) };
    }
    S_OK
}

static VTABLE: Methods<22> = Methods([
    missing::<0> as *const (),
    add_ref as *const (),
    add_ref as *const (),
    missing::<3> as *const (),       // 0x18 closed captions
    missing::<4> as *const (),       // 0x20
    missing::<5> as *const (),       // 0x28 high contrast
    missing::<6> as *const (),       // 0x30 speech to text
    missing::<7> as *const (),       // 0x38
    enumerate_voices as *const (),   // 0x40
    create as *const (),             // 0x48
    close_synth as *const (),        // 0x50
    set_default_voice as *const (),  // 0x58
    set_custom_voice as *const (),   // 0x60
    create_stream as *const (),      // 0x68 from text
    close_stream as *const (),       // 0x70 a stream
    stream_size as *const (),        // 0x78
    stream_data as *const (),        // 0x80
    missing::<17> as *const (),      // 0x88 speech to text
    missing::<18> as *const (),      // 0x90
    missing::<19> as *const (),      // 0x98
    missing::<20> as *const (),      // 0xa0
    create_ssml_stream as *const (), // 0xa8 from SSML
]);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::xgameruntime::method;

    extern "system" fn count_voice(info: *const VoiceInformation, ctx: *mut c_void) -> bool {
        // SAFETY: passed from `streams_and_voices_produce_valid_wave_files`.
        unsafe {
            assert!(!(*info).voice_id.is_null());
            *(ctx.cast::<u32>()) += 1;
        }
        true
    }

    #[test]
    fn streams_are_empty_wave_files() {
        let this: This = &OBJECT;
        type EnumVoices = extern "system" fn(This, *mut c_void, *const c_void) -> HRESULT;
        type Create = extern "system" fn(This, *mut *const u8) -> HRESULT;
        type Close = extern "system" fn(This, *const u8) -> HRESULT;
        type SetCustom = extern "system" fn(This, *const u8, *const c_char) -> HRESULT;
        type Stream = extern "system" fn(This, *const u8, *const c_char, *mut *const u8) -> HRESULT;
        type Size = extern "system" fn(This, *const u8, *mut usize) -> HRESULT;
        type Data = extern "system" fn(This, *const u8, usize, *mut u8, *mut usize) -> HRESULT;

        let mut voices = 0u32;
        assert_eq!(
            method::<EnumVoices>(&OBJECT, 0x40)(
                this,
                (&raw mut voices).cast(),
                count_voice as *const c_void,
            ),
            S_OK
        );
        assert_eq!(voices, 1);

        let (mut synth, mut stream, mut size) = (std::ptr::null(), std::ptr::null(), 0);
        assert_eq!(method::<Create>(&OBJECT, 0x48)(this, &mut synth), S_OK);
        assert_eq!(
            method::<SetCustom>(&OBJECT, 0x60)(this, synth, c"default".as_ptr()),
            S_OK
        );
        let say = method::<Stream>(&OBJECT, 0x68);
        assert_eq!(say(this, synth, c"Hello".as_ptr(), &mut stream), S_OK);
        assert_eq!(method::<Size>(&OBJECT, 0x78)(this, stream, &mut size), S_OK);
        assert!(size >= 44);
        let mut wav = vec![0u8; size];
        let mut used = 0;
        let data = method::<Data>(&OBJECT, 0x80);
        assert_eq!(data(this, stream, size, wav.as_mut_ptr(), &mut used), S_OK);
        assert_eq!(
            (&wav[..4], &wav[8..16], &wav[36..40]),
            (&b"RIFF"[..], &b"WAVEfmt "[..], &b"data"[..])
        );
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(used, size);
        assert_eq!(
            u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize + 8,
            size
        );
        assert_eq!(method::<Close>(&OBJECT, 0x70)(this, stream), S_OK);

        // SSML synthesis (0xa8) and empty text fallback.
        let say_ssml = method::<Stream>(&OBJECT, 0xa8);
        let mut ssml_stream = std::ptr::null();
        assert_eq!(
            say_ssml(
                this,
                synth,
                c"<speak version='1.0'>Ready</speak>".as_ptr(),
                &mut ssml_stream
            ),
            S_OK
        );
        let mut ssml_size = 0;
        assert_eq!(
            method::<Size>(&OBJECT, 0x78)(this, ssml_stream, &mut ssml_size),
            S_OK
        );
        assert!(ssml_size >= 44);
        assert_eq!(method::<Close>(&OBJECT, 0x70)(this, ssml_stream), S_OK);
        assert_eq!(method::<Close>(&OBJECT, 0x50)(this, synth), S_OK);
    }
}
