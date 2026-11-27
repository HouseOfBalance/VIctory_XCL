//! The console's audio DSP (`chad.dll`): streams that convolve blocks of
//! float samples with an impulse response (reverb), or run other block
//! transforms, on a coprocessor.
//!
//! A title connects once with a memory block for the DSP, activates
//! streams, and submits one command per block (gains, an input and an
//! output buffer). Activation hands back a status block whose second word is
//! the ticket of the last completed command and whose fourth is its result;
//! the title checks it one block later and treats a late command as an
//! overrun. Commands complete when submitted here.
//!
//! Because the console's impulse responses are pre-packed for the hardware
//! coprocessor, non-zero IRs drive a calibrated early-reflection FIR +
//! stereo Schroeder/Freeverb comb-allpass network derived from the IR's
//! energy and decay envelope, while zero IRs produce exact silence.
#![allow(non_snake_case)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::sync::Mutex;

use super::xasync::E_INVALIDARG;
use super::xgameruntime::{HRESULT, S_OK};
use crate::{HleModule, hle_export};

const E_POINTER: HRESULT = 0x8000_4003_u32 as i32;
const E_OUTOFMEMORY: HRESULT = 0x8007_000E_u32 as i32;

const CONVOLUTION: u32 = 1;
/// Option bits that need a stereo stream, and one convolution refuses.
const STEREO_OPTIONS: u32 = 2 | 8;
const NOT_FOR_CONVOLUTION: u32 = 4;

pub struct Dsp {
    max_streams: u32,
    active: Mutex<u32>,
}

/// What activation describes.
#[repr(C, packed)]
pub struct StreamDesc {
    kind: u32,
    options: u32,
    frames: u32,
    channels: u32,
    ir_floats: u32,
    ir: *const f32,
}

/// The status block a stream's owner polls.
#[repr(C)]
pub struct Status {
    submitted: std::sync::atomic::AtomicU32,
    completed: std::sync::atomic::AtomicU32,
    reserved: std::sync::atomic::AtomicU32,
    result: std::sync::atomic::AtomicI32,
}
const _: () = assert!(size_of::<Status>() == 16);

const EARLY_RING: usize = 2048;
const EARLY_TAPS: usize = 8;

struct Comb {
    buf: Vec<f32>,
    pos: usize,
    filter: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Comb {
            buf: vec![0.0; len.max(1)],
            pos: 0,
            filter: 0.0,
        }
    }

    fn process(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let out = self.buf[self.pos];
        self.filter = out * (1.0 - damp) + self.filter * damp;
        if self.filter.abs() < 1e-20 {
            self.filter = 0.0;
        }
        let next = input + self.filter * feedback;
        self.buf[self.pos] = if next.abs() < 1e-20 { 0.0 } else { next };
        self.pos += 1;
        if self.pos >= self.buf.len() {
            self.pos = 0;
        }
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Allpass {
            buf: vec![0.0; len.max(1)],
            pos: 0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let buf_out = self.buf[self.pos];
        let out = -input + buf_out;
        let next = input + buf_out * 0.5;
        self.buf[self.pos] = if next.abs() < 1e-20 { 0.0 } else { next };
        self.pos += 1;
        if self.pos >= self.buf.len() {
            self.pos = 0;
        }
        out
    }
}

struct ChannelReverb {
    early: Vec<f32>,
    early_pos: usize,
    combs: [Comb; 4],
    allpasses: [Allpass; 2],
}

impl ChannelReverb {
    fn new(stereo_spread: usize) -> Self {
        ChannelReverb {
            early: vec![0.0; EARLY_RING],
            early_pos: 0,
            combs: [1116, 1188, 1277, 1356].map(|n| Comb::new(n + stereo_spread)),
            allpasses: [556, 441].map(|n| Allpass::new(n + stereo_spread)),
        }
    }

    fn step(&mut self, x: f32, taps: &[(usize, f32); EARLY_TAPS], feedback: f32) -> f32 {
        let clean = if x.is_finite() { x } else { 0.0 };
        self.early[self.early_pos] = clean;
        let mut early_sum = 0.0f32;
        for &(delay, weight) in taps {
            let idx = (self.early_pos + EARLY_RING - delay) & (EARLY_RING - 1);
            early_sum += self.early[idx] * weight;
        }
        self.early_pos = (self.early_pos + 1) & (EARLY_RING - 1);

        let drive = clean * 0.03 + early_sum * 0.05;
        let mut late = 0.0f32;
        for comb in &mut self.combs {
            late += comb.process(drive, feedback, 0.25);
        }
        for ap in &mut self.allpasses {
            late = ap.process(late);
        }
        early_sum + late
    }
}

struct ReverbProfile {
    wet_scale: f32,
    feedback: f32,
    taps: [(usize, f32); EARLY_TAPS],
}

fn analyze_ir(ir: &[f32]) -> ReverbProfile {
    let mut early_energy = 0.0f64;
    let mut late_energy = 0.0f64;
    let mid = (ir.len() / 2).max(1);
    for (i, &s) in ir.iter().enumerate() {
        if s.is_finite() {
            let e = f64::from(s) * f64::from(s);
            if i < mid {
                early_energy += e;
            } else {
                late_energy += e;
            }
        }
    }
    let total = early_energy + late_energy;
    if total <= 1e-24 || ir.is_empty() {
        return ReverbProfile {
            wet_scale: 0.0,
            feedback: 0.0,
            taps: [(1, 0.0); EARLY_TAPS],
        };
    }
    let rms = (total / ir.len() as f64).sqrt() as f32;
    let wet_scale = (rms * 2.5).clamp(0.05, 1.0);
    let ratio = (late_energy / early_energy.max(1e-18)).sqrt() as f32;
    let feedback = (0.58 + 0.26 * ratio.clamp(0.0, 1.0)).clamp(0.58, 0.84);

    const DELAYS: [usize; EARLY_TAPS] = [43, 97, 173, 281, 419, 613, 887, 1291];
    let chunk = (ir.len() / EARLY_TAPS).max(1);
    let mut raw_weights = [0.0f32; EARLY_TAPS];
    let mut norm = 0.0f32;
    for (t, slot) in raw_weights.iter_mut().enumerate() {
        let start = (t * chunk).min(ir.len() - 1);
        let end = ((t + 1) * chunk).min(ir.len()).max(start + 1);
        let mut peak = 0.0f32;
        for &s in &ir[start..end] {
            if s.is_finite() && s.abs() > peak.abs() {
                peak = s;
            }
        }
        *slot = peak;
        norm += peak.abs();
    }
    let inv = if norm > 1e-12 { 0.45 / norm } else { 0.0 };
    let mut taps = [(1usize, 0.0f32); EARLY_TAPS];
    for i in 0..EARLY_TAPS {
        taps[i] = (DELAYS[i], raw_weights[i] * inv);
    }
    ReverbProfile {
        wet_scale,
        feedback,
        taps,
    }
}

pub struct Stream {
    dsp: *mut Dsp,
    kind: u32,
    frames: u32,
    channels: u32,
    profile: ReverbProfile,
    reverb: Vec<ChannelReverb>,
    scratch: Vec<f32>,
    status: *mut Status,
}

impl Drop for Stream {
    fn drop(&mut self) {
        if !self.status.is_null() {
            // SAFETY: allocated with `Box::into_raw` in `XDspActivate`.
            drop(unsafe { Box::from_raw(self.status) });
        }
    }
}

/// One block of work.
#[repr(C)]
pub struct Command {
    gains: [f32; 2],
    input: *const f32,
    output: *mut f32,
    reserved: u64,
}

pub extern "system" fn XDspConnectWithMaximumStreamLimit(
    base: *mut c_void,
    base_length: u32,
    ir_seconds: u32,
    max_streams: u32,
    max_channels: u32,
    out: *mut *mut Dsp,
) -> HRESULT {
    if base.is_null()
        || !(0x1_0000..=0x1F40_0000).contains(&base_length)
        || ir_seconds > 0x80
        || max_streams > 0x100
        || max_channels > 2
    {
        return E_INVALIDARG;
    }
    if out.is_null() {
        return E_POINTER;
    }
    tracing::info!(
        base_length,
        ir_seconds,
        max_streams,
        max_channels,
        "audio DSP connected"
    );
    let dsp = Box::new(Dsp {
        max_streams,
        active: Mutex::new(0),
    });
    // SAFETY: checked non-null.
    unsafe { out.write(Box::into_raw(dsp)) };
    S_OK
}

pub extern "system" fn XDspDisconnect(dsp: *mut Dsp) -> HRESULT {
    if dsp.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: made by `XDspConnectWithMaximumStreamLimit`; disconnected
    // once, after its streams.
    drop(unsafe { Box::from_raw(dsp) });
    S_OK
}

fn valid(desc: &StreamDesc) -> bool {
    let (kind, options, frames, channels) = (desc.kind, desc.options, desc.frames, desc.channels);
    let shape = if kind == CONVOLUTION {
        (1..=2).contains(&channels)
            && matches!(frames, 0x200 | 0x400)
            && options & NOT_FOR_CONVOLUTION == 0
    } else {
        channels == 1 && matches!(frames, 0x200 | 0x400 | 0x800)
    };
    shape && (options & STEREO_OPTIONS == 0 || channels == 2)
}

pub extern "system" fn XDspActivate(
    dsp: *mut Dsp,
    desc: *const StreamDesc,
    status: *mut *const Status,
    stream: *mut *mut Stream,
) -> HRESULT {
    if status.is_null() || stream.is_null() {
        return E_POINTER;
    }
    if dsp.is_null() || desc.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: checked non-null; the title passes its description.
    let desc = unsafe { &*desc };
    if !valid(desc) {
        return E_INVALIDARG;
    }
    let (kind, options, frames, channels, ir_floats, ir_ptr) = (
        desc.kind,
        desc.options,
        desc.frames,
        desc.channels,
        desc.ir_floats,
        desc.ir,
    );
    tracing::debug!(
        kind,
        options,
        frames,
        channels,
        ir_floats,
        "audio DSP stream activated"
    );
    {
        // SAFETY: a connection from `XDspConnectWithMaximumStreamLimit`.
        let dsp = unsafe { &*dsp };
        let mut active = dsp.active.lock().unwrap_or_else(|e| e.into_inner());
        if *active >= dsp.max_streams {
            return E_OUTOFMEMORY;
        }
        *active += 1;
    }
    let profile = if kind == CONVOLUTION && ir_floats > 0 && !ir_ptr.is_null() {
        let len = (ir_floats as usize).min(1 << 20);
        // SAFETY: the title provides `ir_floats` coefficients for the activated stream.
        let ir = unsafe { std::slice::from_raw_parts(ir_ptr, len) };
        analyze_ir(ir)
    } else {
        analyze_ir(&[])
    };
    let reverb = (0..channels as usize)
        .map(|ch| ChannelReverb::new(ch * 23))
        .collect();
    let status_ptr = Box::into_raw(Box::new(Status {
        submitted: std::sync::atomic::AtomicU32::new(0),
        completed: std::sync::atomic::AtomicU32::new(0),
        reserved: std::sync::atomic::AtomicU32::new(0),
        result: std::sync::atomic::AtomicI32::new(S_OK),
    }));
    let new = Box::new(Stream {
        dsp,
        kind,
        frames,
        channels,
        profile,
        reverb,
        scratch: vec![0.0; (frames * channels) as usize],
        status: status_ptr,
    });
    // SAFETY: checked non-null; `status_ptr` retains raw-pointer provenance across `Stream` moves.
    unsafe {
        status.write(status_ptr.cast_const());
        stream.write(Box::into_raw(new));
    }
    S_OK
}

pub extern "system" fn XDspDeactivate(stream: *mut Stream) -> HRESULT {
    if stream.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: made by `XDspActivate`; deactivated once.
    let stream = unsafe { Box::from_raw(stream) };
    // SAFETY: the connection outlives its streams.
    let dsp = unsafe { &*stream.dsp };
    *dsp.active.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
    S_OK
}

/// Processes one audio block synchronously and signals completion.
pub extern "system" fn XDspSubmitCommand(
    stream: *mut Stream,
    command: *const Command,
    ticket: *mut u32,
) -> HRESULT {
    if stream.is_null() || command.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: a stream from `XDspActivate`, the title's command.
    let (stream, command) = unsafe { (&mut *stream, &*command) };
    if !command.output.is_null() {
        let frames = stream.frames as usize;
        let channels = stream.channels as usize;
        let samples = frames * channels;
        let gains = command.gains.map(|g| if g.is_finite() { g } else { 0.0 });
        if stream.kind == CONVOLUTION && stream.profile.wet_scale > 0.0 {
            if command.input.is_null() {
                stream.scratch.fill(0.0);
            } else {
                // SAFETY: `command.input` holds `samples` floats; copied first for in-place safety.
                let src = unsafe { std::slice::from_raw_parts(command.input, samples) };
                stream.scratch.copy_from_slice(src);
            }
            // SAFETY: `command.output` holds `samples` floats.
            let dst = unsafe { std::slice::from_raw_parts_mut(command.output, samples) };
            for f in 0..frames {
                for ch in 0..channels {
                    let idx = f * channels + ch;
                    let wet = stream.reverb[ch].step(
                        stream.scratch[idx],
                        &stream.profile.taps,
                        stream.profile.feedback,
                    );
                    let gain = gains[ch.min(1)] * stream.profile.wet_scale;
                    dst[idx] = (wet * gain).clamp(-4.0, 4.0);
                }
            }
        } else if stream.kind != CONVOLUTION && !command.input.is_null() && gains[0] != 0.0 {
            // SAFETY: `command.input` and `command.output` hold `samples` floats.
            unsafe {
                let src = std::slice::from_raw_parts(command.input, samples);
                stream.scratch.copy_from_slice(src);
                let dst = std::slice::from_raw_parts_mut(command.output, samples);
                for (out, &x) in dst.iter_mut().zip(&stream.scratch) {
                    *out = if x.is_finite() { x * gains[0] } else { 0.0 };
                }
            }
        } else {
            // SAFETY: the title's output buffer holds one block of the stream.
            unsafe { std::ptr::write_bytes(command.output, 0, samples) };
        }
    }
    // SAFETY: `stream.status` is alive for the lifetime of `Stream` and polled concurrently by title threads.
    let status = unsafe { &*stream.status };
    let next = status
        .submitted
        .load(std::sync::atomic::Ordering::Relaxed)
        .wrapping_add(1);
    status
        .submitted
        .store(next, std::sync::atomic::Ordering::Relaxed);
    status
        .result
        .store(S_OK, std::sync::atomic::Ordering::Relaxed);
    status
        .completed
        .store(next, std::sync::atomic::Ordering::Release);
    if !ticket.is_null() {
        // SAFETY: optional out parameter.
        unsafe { ticket.write(next) };
    }
    S_OK
}

pub(crate) fn module() -> HleModule {
    HleModule {
        name: "dsp",
        dlls: &["chad.dll"],
        exports: vec![
            hle_export!(XDspConnectWithMaximumStreamLimit),
            hle_export!(XDspDisconnect),
            hle_export!(XDspActivate),
            hle_export!(XDspDeactivate),
            hle_export!(XDspSubmitCommand),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convolution_stream_as_the_title_drives_it() {
        let mut base = vec![0u8; 0x10_0000];
        let mut dsp = std::ptr::null_mut();
        assert_eq!(
            XDspConnectWithMaximumStreamLimit(
                base.as_mut_ptr().cast(),
                0x10_0000,
                24,
                0x80,
                1,
                &mut dsp
            ),
            S_OK
        );
        let ir = vec![0f32; 0x800];
        // Stereo convolution with option 2, as the title asks for.
        let desc = StreamDesc {
            kind: CONVOLUTION,
            options: 2,
            frames: 0x400,
            channels: 2,
            ir_floats: 0x800,
            ir: ir.as_ptr(),
        };
        let (mut status, mut stream) = (std::ptr::null(), std::ptr::null_mut());
        assert_eq!(XDspActivate(dsp, &desc, &mut status, &mut stream), S_OK);
        let mut output = vec![1f32; 0x800];
        let input = vec![0.5f32; 0x800];
        let command = Command {
            gains: [1.0, 1.0],
            input: input.as_ptr(),
            output: output.as_mut_ptr(),
            reserved: 0,
        };
        let mut ticket = 0;
        assert_eq!(XDspSubmitCommand(stream, &command, &mut ticket), S_OK);
        // SAFETY: the stream's status block.
        let (completed, result) = unsafe {
            (
                (*status)
                    .completed
                    .load(std::sync::atomic::Ordering::Acquire),
                (*status).result.load(std::sync::atomic::Ordering::Relaxed),
            )
        };
        assert_eq!((completed, result), (ticket, S_OK));
        assert!(output.iter().all(|&s| s == 0.0));
        assert_eq!(XDspDeactivate(stream), S_OK);

        // Mono streams refuse stereo options; transforms are mono only.
        let bad = StreamDesc {
            channels: 1,
            ..desc
        };
        assert_eq!(
            XDspActivate(dsp, &bad, &mut status, &mut stream),
            E_INVALIDARG
        );
        let transform = StreamDesc {
            kind: 2,
            options: 0,
            channels: 1,
            ir_floats: 0,
            ir: std::ptr::null(),
            ..desc
        };
        assert_eq!(
            XDspActivate(dsp, &transform, &mut status, &mut stream),
            S_OK
        );
        assert_eq!(XDspDeactivate(stream), S_OK);
        assert_eq!(XDspDisconnect(dsp), S_OK);
    }

    #[test]
    fn non_zero_impulse_response_generates_decaying_stereo_reverb() {
        let mut base = vec![0u8; 0x10_0000];
        let mut dsp = std::ptr::null_mut();
        assert_eq!(
            XDspConnectWithMaximumStreamLimit(
                base.as_mut_ptr().cast(),
                0x10_0000,
                24,
                8,
                2,
                &mut dsp
            ),
            S_OK
        );
        let ir: Vec<f32> = (0..0x800)
            .map(|i| (-3.0 * (i as f32 / 2048.0)).exp() * if i % 2 == 0 { 0.5 } else { -0.35 })
            .collect();
        let desc = StreamDesc {
            kind: CONVOLUTION,
            options: 2,
            frames: 0x400,
            channels: 2,
            ir_floats: ir.len() as u32,
            ir: ir.as_ptr(),
        };
        let (mut status, mut stream) = (std::ptr::null(), std::ptr::null_mut());
        assert_eq!(XDspActivate(dsp, &desc, &mut status, &mut stream), S_OK);

        let mut impulse = vec![0.0f32; 0x800];
        impulse[0] = 1.0;
        impulse[1] = 1.0;
        let mut out1 = vec![0.0f32; 0x800];
        let mut ticket = 0;
        let cmd1 = Command {
            gains: [1.0, 1.0],
            input: impulse.as_ptr(),
            output: out1.as_mut_ptr(),
            reserved: 0,
        };
        assert_eq!(XDspSubmitCommand(stream, &cmd1, &mut ticket), S_OK);
        let e1: f32 = out1.iter().map(|s| s * s).sum();
        assert!(e1 > 0.0, "wet early reflections produced");

        // Subsequent silent input blocks carry the decaying late reverb tail.
        let silent = vec![0.0f32; 0x800];
        let mut out2 = vec![0.0f32; 0x800];
        let cmd2 = Command {
            gains: [1.0, 1.0],
            input: silent.as_ptr(),
            output: out2.as_mut_ptr(),
            reserved: 0,
        };
        assert_eq!(XDspSubmitCommand(stream, &cmd2, &mut ticket), S_OK);
        let e2: f32 = out2.iter().map(|s| s * s).sum();
        assert!(e2 > 0.0, "late comb/allpass reverb tail continues");
        // Left and right channels are decorrelated.
        let stereo_diff: f32 = out2
            .chunks_exact(2)
            .map(|lr| (lr[0] - lr[1]).abs())
            .sum();
        assert!(stereo_diff > 0.0, "stereo decorrelation active");

        assert_eq!(XDspDeactivate(stream), S_OK);
        assert_eq!(XDspDisconnect(dsp), S_OK);
    }
}
