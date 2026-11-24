# VIctory

Runs Grand Theft Auto VI (Xbox Series X|S) natively on Windows, from files
the user supplies.

The title's x86-64 code runs unchanged inside our process; VIctory supplies
the platform underneath it and converts the title's data, ahead of time where
it can and on first use where it must (shaders). See
[docs/architecture.md](docs/architecture.md).

## Status (2026-11-23)

**Not playable yet.** The title boots, runs its menus at its own 30 fps and
loads the world, where rendering fails.

| Stage | State |
|-------|-------|
| Boot, first-boot screens (accessibility, language, settings, legal) | works, 30 fps |
| Intro video with sound | works |
| Start screen, profile and saves | works |
| Loading into the world (about 50 s) | works |
| The world (first mission's intro) | reached and runs with indirect draws; its title card renders, the scene after it is still dark; 0.5-1.8 s per frame |

Tested on one machine: Windows 11, AMD RX 6700 XT.

> **Warning.** Entering the world on a real GPU can currently hang it, and
> Windows then resets the display driver (every monitor goes black for a
> moment). To look further, use a diagnostics build with
> `VICTORY_D3D12_WARP=1`, which renders on the software adapter.

What each part does today and what it still needs:

| Subsystem | Today | Needs |
|-----------|-------|-------|
| Loader | Runs the title's executable and its DLLs in our process; emulates Zen 2 CPU instructions (`clzero`, `monitorx`/`mwaitx`, `rdpru`, `rdpid`, `clwb`, `sse4a`) on Intel CPUs; crash reports name the faulting code and are kept in `crashes.log` | – |
| Runtime services | One signed-in offline user, licences, packages, saves that survive a kill at any step, text entry dialog, SAPI 5 speech synthesis (text & SSML) and PNG screenshots | The dialog, speech and relaunch are untested live |
| Network | Offline by design: the title cannot reach the network | – |
| Files and streaming | Archives and streamed data read, zlib and BCPack decoded in parallel | Loading is limited by writes into title memory |
| Input | XInput pads, 4-motor trigger rumble (`Windows.Gaming.Input` with XInput fallback), keyboard + mouse stand-in pad (mouse camera look, LMB/RMB triggers, `F9` cursor lock) | – |
| Audio | Menu, video and loading sound; XMA voices, Opus streams and IR-calibrated stereo reverb DSP (`chad.dll`) | Opus and reverb results unverified against the title |
| GPU: menus | Draws, compute and presentation on host D3D12 (1080p/1440p/4K 60/120/144 Hz modes, VRR tearing-free unlocked presentation, HDR swap-chain color spaces, DLSS / Catmull-Rom + RCAS display upscaling, texture read-back, 16-slot frame event scheduling with offsets, console DXGI format mapping); only changed title memory is copied | Frame pacing drifts on one screen in release builds |
| GPU: world | Mesh pipelines (direct & indirect), shader-written buffers, placed SRV/UAV textures, descriptor lifetimes, indirect execution (arguments clamped), D3D12 query heaps & depth bounds, priority worker-thread pipelines (up to 16 threads) kept on disk | Merged geometry stages still drop some world draws; frames remain heavy |
| Shaders | Console GPU code (`DXIL`/`FSSM` and AMD FSR 4 `ACCC`/`FSPX` containers) recompiled to DXIL at first use — including `image_bvh(64)_intersect_ray` BVH4 ray-tracing lowering, SSA SGPR spill-lane recovery (`0..63`) for Wave32 GPUs and VS/PS/MS, 64-bit image/buffer/LDS atomics, narrow 8/16-bit LDS accesses, MSAA image loads, implicit/comparison `image_gather4`, `a16`/`g16` sample addressing, compressed `f16x2` exports, `SV_RenderTargetArrayIndex`/`SV_ViewportArrayIndex`, line mesh shaders, and 1–4 channel UAV stores — cached on disk | Merged geometry stages; compute programs with genuine cross-lane wave64 shuffles need a 64-lane GPU |
| Textures | Console tiled layouts, detiling and render-target/depth metadata planes (`DCC`, `FMASK`, `CMASK`, `HTILE`) for the modes the title uses | – |
| Series S mode | Runs through the menus to the loading screen | The world, as above |

## Getting started

Requirements: Windows 10/11 x64, a CPU with AVX2, a Direct3D 12 GPU with
shader model 6.6 (and mesh shaders for the world), Rust (pinned in
`rust-toolchain.toml`).

The title also needs several GiB of available Windows commit at startup.
Check **Committed** under **Task Manager → Performance → Memory**. If
`crashes.log` reports Windows error 1455, close other applications or increase
the Windows paging file size, then retry.

```powershell
cargo xtask install-hooks                # asset-policy check on every commit
copy victory.example.toml victory.toml   # then set your paths (outside the repo)
cargo test --workspace                   # needs no game data
cargo run -p victory -- config           # check your paths
```

Run the title:

```powershell
cargo build --release -p victory
target\release\victory.exe run <game dir>\<the title's exe> --execute
```

The window caption shows the framerate. Saves, settings and `crashes.log`
are kept under the data folder (default `%LOCALAPPDATA%\victory\data`), derived
data under the cache folder.

Input: XInput pads. With none connected the keyboard is a pad while the
title's window has focus:

| Keys | Pad |
|------|-----|
| W A S D / Mouse (or I J K L) | left / right stick |
| Left / Right Mouse (or C / Z) | right / left trigger |
| Middle Mouse / F / H | right / left stick click |
| F9 | toggle mouse cursor capture |
| arrows | D-pad |
| Enter, Backspace, X, Y | A, B, X, Y |
| Q / E | shoulders |
| Esc, Tab | Menu, View |

## Commands

```
victory inspect <exe|dll> [--imports] [--exports]
victory census [paths...] [--missing] [--strict]
victory lib-exports <import.lib> [--list]
victory shader-info [paths...] [--verbose] [--disasm]
victory translate [paths...] [--validate] [--pipelines] [--list] [--jobs N]
victory run <exe> [--execute] [--unimplemented trap|zero] [--strict]
            [--console series-x|series-s] [-- <title arguments>]
victory config
```

Logging: `VICTORY_LOG=victory_loader=trace,info` (a release build logs
info and above).

## Display, Upscaling & High-Refresh Configuration

Release and diagnostics builds accept these runtime environment variables:

| Variable | Effect |
|----------|--------|
| `VICTORY_UPSCALER=<mode>` | `auto` (default), `cas`, `dlss` / `dlss-quality`, `dlss-balanced`, `dlss-performance`, `dlss-ultra-performance`, or `dlss-dlaa` (loads NVIDIA NGX dynamically from the driver or `VICTORY_NGX_DLL`, with D3D12 Catmull-Rom + RCAS fallback on all GPUs) |
| `VICTORY_OUTPUT_RES=<WxH>` | target output swap-chain resolution, e.g. `3840x2160` or `2560x1440` |
| `VICTORY_SHARPNESS=<0.0..1.0>` | upscaler contrast-adaptive sharpening strength (default `0.55`) |
| `VICTORY_TARGET_FPS=<fps>` | overrides console frame pacing interval, e.g. `120`, `144`, or `unlocked` |
| `VICTORY_VSYNC=1` | forces vertical sync instead of low-latency `ALLOW_TEARING` VRR presentation |
| `VICTORY_MOUSE_CAPTURE=1` | locks mouse cursor to the game window for camera look (toggleable with `F9`) |
| `VICTORY_PIPELINE_WORKERS=<1..16>` | overrides background D3D12 pipeline compilation worker count |

## Debugging

A release build has no debugging aids. Build them in with

```powershell
cargo build --profile diag -p victory --features diagnostics   # target\diag\victory.exe
```

| Variable | Effect |
|----------|--------|
| `VICTORY_LOG=<target>=debug,...` | debug and trace output |
| `VICTORY_STACKS=20,60` | every thread's stack at those seconds |
| `VICTORY_WATCH=<rva>,...` | logs a title u32 when it changes |
| `VICTORY_KEYS=30:Enter,35:X` | presses keys at those seconds, focus or not |
| `VICTORY_CRASH_SAVE=<rva>:<len>,...` | saves title memory to the cache folder on a crash |
| `VICTORY_CAPTURE=<dir>` | saves presented frames (powers of two, or `VICTORY_CAPTURE_FRAMES=<list>`) |
| `VICTORY_DUMP_TARGETS=1` | with the two above, saves every render target of those frames |
| `VICTORY_D3D12_DEBUG=1` | host debug layer |
| `VICTORY_D3D12_GBV=1` | with the above, GPU-based validation (bad descriptor use reported, not faulted; slow) |
| `VICTORY_D3D12_DRED=1` | on a GPU failure, the allocation hit and the commands in flight |
| `VICTORY_SKIP_PIPELINES=<hash>,...` | omit draws and dispatches, including indirect ones, whose pipeline contains a listed 16-digit shader hash |
| `VICTORY_GPU_TIMING=1` | with the above, each list of a submission run and timed on its own; slow lists logged with their slowest commands |
| `VICTORY_D3D12_WARP=1` | host device on the software adapter (slow; cannot reset the driver) |
| `VICTORY_SHADER_LOOP_LIMIT=<n>` | translated shader loops stop after `n` iterations per lane instead of 2^20 (own shader cache entries); if a lower `n` makes world frames much faster, loops hitting the limit are what makes them slow |
| `VICTORY_AUDIO_CAPTURE=<file.wav>` | records what the title plays (our process only) |
| `VICTORY_XMA_DUMP=<dir>` | saves the first 32 XMA voices' context and input |

Every build appends crashes and host GPU failures to `crashes.log` in the
title's data folder.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
