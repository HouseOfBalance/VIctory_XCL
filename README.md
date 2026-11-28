# VIctory_XCL

Unified Xbox Series X|S native PC compatibility layer combining [avatarmanz/VIctory-7900XTX](https://github.com/avatarmanz/VIctory-7900XTX) (multi-threaded `VICTORY_THIN` command recording, `VICTORY_ASYNC_SUBMIT` flight pipelining, dedicated D3D12 copy queue, 99.25% shader coverage, zero-copy renderer port patches), [trustno1-coder/VIctory-NVIDIA](https://github.com/trustno1-coder/VIctory-NVIDIA), [fxceless/victory-fork](https://github.com/fxceless/victory-fork), and [DexNom/VIctory](https://github.com/DexNom/VIctory) with **NVIDIA Streamline temporal DLSS Super Resolution + DLSS Frame Generation**, **AMD FSR 3 Frame Generation**, **native 2560x1440 SDR output**, **in-game `F1` settings menu & `F3` stats panel**, **120/144 Hz VRR & HDR10/scRGB presentation**, **Wave32 water-tile & BVH prefix-sum fixes**, **2048-point FFT convolution + stereo reverb DSP**, **native Sony DualSense / DualShock 4 HID + 4-motor Impulse Trigger rumble + `MouseLook`**, and **D3D12 Agility SDK auto-matching**.

## The goal

**A stable 30 fps or more without ray tracing, then as high a frame rate and as
low frame times as the PC allows**, on an AMD RX 7900 XTX first and on PCs with
32 GB (later 16 GB) of RAM.

The way there is **porting, not emulating**: patching the title's renderer so it
behaves the way a PC renderer does, rather than having the layer watch and copy
the console's shared memory every frame. Shaders are translated ahead of time
into a versioned store that is kept between builds. Ray tracing stays optional
and off by default. The picture comes after the frame rate: texture pop-in and
lighting that is too bright or too dark when walking indoors are next in line.

**This repository contains no game files, code or assets.** You need your own
copy of the game. It is not affiliated with or endorsed by the game's developer
or publisher.

## Where it stands (2026-11-26)

Tested on one machine only: Windows 11, Intel i9-12900K, 64 GB RAM, Radeon
RX 7900 XTX, ray tracing off.

| | |
|---|---|
| City street, Series S profile, renderer patches 1, 2, 5, 6 and 7 on | about 45 ms a frame (about 22 fps, 44.9 ms in run merge-5) |
| Same, patches 1, 2, 5 and 6 | 45.6 ms, p95 47 ms |
| Same, patches 1, 2 and 5 | 47.3 ms (about 21 fps), p95 49 ms |
| Series X profile, patches 1+2 | about 53 ms (about 19 fps) |
| Same scene before the patches | 57-75 ms (13-17 fps) |
| Shaders the title uses that translate | all of them (25,790 of 25,985; the rest are unused geometry shaders) |
| Memory | about 30 GB of private memory, 10.5 GB of VRAM: **64 GB of RAM recommended** for now |

It started at 280 ms a frame in the same scenes three days earlier. What sets
the frame now is the one thread that submits the title's work to the card
(about 40 ms); the card itself needs about 36 ms and now waits about 7 ms a frame with nothing queued.

### Renderer patches and frame-time defaults (on by default)

All five renderer ports and the multi-threaded submission/draw-batching paths
below are **on by default** outside unit tests; pass `=0` (or
`scripts\play.ps1 -Safe`) to turn any of them off when diagnosing an issue.

| Switch | What it does |
|---|---|
| `VICTORY_PORT_CONSTANTS=0` | Turns off renderer port #1 (by default the title's 174 MB constants heap is read by the card where the title writes it: no uploads, no fills) |
| `VICTORY_PORT_DESCRIPTORS=0` | Turns off renderer port #2 (by default the title's descriptor-table stores are byte-checked and patched to report written slots instead of scanning ~600,000 slots a frame) |
| `VICTORY_PORT_CBV=0` | Turns off renderer port #5 (by default translated shaders read constant buffers in place as raw bytes: ~22,000 fills a frame to none; -4.7 ms) |
| `VICTORY_PORT_GUARD=0` | Turns off renderer port #6 (by default the write guards' lock is shared by faults and refreshes per 64 KiB block, cutting title-thread waits from 29 ms to ~2 ms) |
| `VICTORY_PORT_JOURNAL=0` | Turns off renderer port #7 (by default heap records are asked at submit and the journal is sharded into 64 locks, cutting submit waits from ~6 ms to ~0.3 ms) |
| `VICTORY_THIN=0` | Turns off multi-threaded list recording/translation (8 worker threads by default) |
| `VICTORY_LATE_WAIT=0` | Turns off stage-3 asynchronous flight pipelining and `VICTORY_RELAY=2` background shader-write landing |
| `VICTORY_JOIN_BARRIERS=0` | Turns off coalescing consecutive D3D12 resource barriers per host list |
| `VICTORY_INDIRECT_BATCH=0` | Turns off stage-2 batched indirect argument clamping and `VICTORY_INDIRECT_WINDOW=0` windowed clamping |

### Other switches

| Switch | What it does |
|---|---|
| `--console series-s` | The Series S profile: 1080p output, about 5 ms faster, a softer picture |
| `VICTORY_OUTPUT=1920x1080` | The title renders to that output size itself (Series X profile; about 16 ms less card time) |
| `VICTORY_FRAME_INTERVAL_MS=16.667` | Lets the title go above 30 fps (menus and videos reach 60; the game's clock stays right) |
| `VICTORY_TARGET_FPS=120` | High-frame-rate pacing (`60`, `120`, `144`, or `0`/`unlocked`) for both the host clock and the title's internal limiter |
| `VICTORY_VSYNC=0` | Tear-free VRR (`DXGI_PRESENT_ALLOW_TEARING`) by default; set `1` to force vertical sync |
| `VICTORY_HOST_UPSCALER=dlss-quality` | Host spatial/NGX upscaler fallback (`auto`, `cas`, `dlss`, `dlss-quality`, `dlss-balanced`, `dlss-performance`, `dlss-ultra-performance`, `dlss-dlaa`) |
| `VICTORY_DLSS=1` | NVIDIA Streamline temporal DLSS Super Resolution (`VICTORY_DLSS_MODE=quality`, `VICTORY_DLSS_PRESET=k`) intercepting the title's FidelityFX motion-vector/depth pass |
| `VICTORY_FRAMEGEN=2` | Frame generation (`2`..`6` via Streamline DLSS Frame Generation on RTX 40/50, or `VICTORY_FSR_FRAMEGEN=1` for AMD FSR 3 2x Frame Generation on any D3D12 GPU) |
| `VICTORY_NATIVE_1440P=1` | Patches the supported retail Series X image for native `2560x1440` SDR output (`VICTORY_RENDER_1080P=1` renders scene at `1080p` -> `1440p`) |
| `VICTORY_OUTPUT_RES=3840x2160` | Explicit presentation output resolution for DLSS or D3D12 Catmull-Rom + RCAS spatial upscaling (`VICTORY_SHARPNESS=0.55`) |
| `VICTORY_FULLSCREEN=1` | Borderless fullscreen at startup (`F11` or `Alt+Enter` toggles live; `F1` opens in-game settings menu; `F3` toggles live stats panel) |
| `VICTORY_SKIP_BINK=1` | Skips Bink startup logo movies (`all` skips in-game Bink videos too) |
| `VICTORY_DUALSENSE=1` | Native USB/Bluetooth Sony DualSense, DualSense Edge, and DualShock 4 HID input + rumble alongside XInput/WGI |
| `VICTORY_MOUSE_CAPTURE=1` | Locks cursor to window center for smoothed `MouseLook` right-stick camera + LMB/RMB triggers (`F9` toggles live) |
| `VICTORY_BUFFERS_AHEAD=4096` | Pre-allocates 64 KiB CPU/GPU buffer pools on a background thread and orders static samplers deterministically (`VICTORY_SAMPLER_ORDER=1`) |
| `VICTORY_HOME=D:\victory` | Relocates shader/pipeline/image caches and save data off `C:` |
| `VICTORY_AMBIENT_SETTLE` | How fast light settles after walking indoors (on by default, `250,3`; `0` turns it off) |
| `VICTORY_TEXTURE_STORE=1` | Converted textures kept between runs (about 8 ms a frame for now: off) |

What is known to be wrong:

- Trees, signs and far objects flicker in the distance in some places.
- Lighting is too bright or too dark after walking indoors, until the title has
  run for a while; rooms with little direct light are too dark without `-RayTracing`.
- Textures of what just came into view appear late while the title streams.
- The first run of a new build stutters while the driver makes pipelines
  (`victory warm` makes them ahead).
- Recording a frame in the world makes the title's own watchdog end it.

## How to run it

You need Windows 10/11 x64, a CPU with AVX2, a Direct3D 12 GPU with shader
model 6.6 and mesh shaders, Rust (the toolchain is pinned), the MSVC build
tools, and your own dump of the title.

### Quick start (built-in folder launcher)

No `victory.toml` editing is required. Double-click **`Launch-VIctory.cmd`**
(or drag-and-drop your game folder onto **`Launch-VIctory.cmd`** /
**`victory.exe`**):

1. If `victory.exe` is not on disk yet, `Launch-VIctory.cmd` automatically
   downloads the SHA-256-verified pre-built `victory.exe` from this
   repository's GitHub Releases (no Rust or Visual Studio installation needed;
   falls back to `cargo build` if installed).
2. On the first launch it opens a Windows folder picker — select the folder
   containing your game `.exe` (even if nested inside a dump folder). The path
   is saved under `%LOCALAPPDATA%\victory\game-path.txt` so every later launch
   starts immediately; caches and saves go under `%LOCALAPPDATA%\victory`.
3. The launcher automatically tunes for **4K 120 FPS** according to your GPU:
   - **GeForce RTX 50 series:** `3840x2160` output + **DLSS 4 Preset K
     (`Performance`, `1080p -> 4K`)** + **DLSS Multi-Frame Generation `4x`** +
     `120` FPS target.
   - **GeForce RTX 40 series:** `3840x2160` output + **DLSS 4 Preset K
     (`Performance`)** + **DLSS Frame Generation `2x`** + `120` FPS target.
   - **GeForce RTX 20 / 30 series:** `3840x2160` output + **DLSS 4 Preset K
     (`Performance`)** + **AMD FSR 3 Frame Generation `2x`** + `120` FPS target.
   - **AMD Radeon / Intel Arc:** fast `1080p -> 1440p` internal scene render +
     **D3D12 5-tap Catmull-Rom + RCAS spatial upscaling to `3840x2160`** at
     `120` Hz VRR.
4. Press **`F1`** in-game to open the settings menu (DLSS `Performance` /
   `Ultra Performance` / `Quality`, Frame Generation `2x`–`6x` / FSR 3, FPS
   cap, `3840x2160` / `2560x1440`, shader preload, save & restart) and **`F3`**
   for the live stats panel.

From PowerShell, you can also pass the game folder directly:

```powershell
git clone https://github.com/HouseOfBalance/VIctory_XCL
cd VIctory_XCL
cargo build --locked --profile diag -p victory --features diagnostics
target\diag\victory.exe play "D:\Games\YourGameFolder"
```

Useful launcher commands:

```powershell
target\diag\victory.exe play --pick       # choose a different game folder via Windows dialog
target\diag\victory.exe play --safe       # conservative mode (ports and thin translation off)
target\diag\victory.exe preload           # translate all shaders + warm known pipelines ahead of time
target\diag\victory.exe get-dlss          # fetch official NVIDIA Streamline / DLSS DLLs (~70 MB)
target\diag\victory.exe get-fsr           # fetch official AMD FidelityFX FSR 3 DLLs (~30 MB)
target\diag\victory.exe get-d3d12         # fetch Microsoft D3D12 Agility SDK for Windows 10 (~22 MB)
```

`scripts\play.ps1` also accepts a folder (or opens the folder picker when run
with no arguments); `scripts\play.ps1 -Safe` leaves out the newest, faster ways,
and `-RayTracing` turns the title's ray-traced indirect light on.

On a hybrid-graphics PC, the runtime and offline pipeline checks prefer a
high-performance adapter with Shader Model 6.6. The runtime logs its adapter
name, PCI vendor and device IDs, and dedicated VRAM. Set
`VICTORY_D3D12_ADAPTER` to a zero-based DXGI index or part of the adapter's
name to select it explicitly. With the launcher, pass
`-More @{VICTORY_D3D12_ADAPTER='NVIDIA'}`.

Before the first run, ahead of time (both can be stopped and go on later):

```powershell
target\diag\victory.exe shaders <the shader archives>   # every shader translated, minutes
target\diag\victory.exe warm --used                     # pipelines earlier runs drew with, for a new driver or build
```

Commands, debugging switches and upstream's own description:
[docs/README-fxceless.md](docs/README-fxceless.md). Every `VICTORY_...` switch is
described in a comment where it is read.

## What is different here

In short; the reasons and the measurements are in [docs/findings.md](docs/findings.md).

- **List calls are written down, not carried out, on the title's threads**
  (`VICTORY_THIN`): eight threads of ours translate them.
- **Submissions are prepared by a thread of ours and joined**
  (`VICTORY_ASYNC_SUBMIT`, `VICTORY_JOIN_PARTS`), with what can be spread over
  threads spread: refreshes, compares (AVX2), descriptor records, landings.
- **What shaders wrote comes back beside the write guards**, by the dwords that
  changed, on a thread of its own (`VICTORY_RELAY`, `VICTORY_LAND`).
- **Buffer fills that need bytes still on the card are done on the card**
  (`VICTORY_CARD_STEPS`) instead of waiting for it.
- **The title's writes are caught more cheaply**: one view opened at a fault,
  blocks that rest, places in the title's code that look before they write.
- **Textures go up on a copy queue of their own** and a slot shows one only after
  its upload ran, which also ended a device loss that came every 15 minutes or so.
- **Boats float**: textures that shaders write and the title's CPU reads go back
  to title memory (from fxceless and Geckomacabre, on by default here).
- **Frame record and replay** with per-pass times and shader variants compared
  turn by turn ([docs/replay.md](docs/replay.md)).

## Credits

This is a tree of other people's work with more on top. The history is kept, and
commits taken from elsewhere name the commit and its author.

- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and its
  contributors: the loader, the runtime services, the Direct3D 12 back end and
  the shader recompiler.
- **[fxceless/victory-fork](https://github.com/fxceless/victory-fork)**: the fork
  this one started from, Wave32 water-tile and spilled BVH prefix-sum fixes, recycled ray-tracing fault-page budget, and the people whose pull requests it carries.
- **[trustno1-coder/VIctory-NVIDIA](https://github.com/trustno1-coder/VIctory-NVIDIA)**:
  Streamline DLSS Super Resolution and Frame Generation, AMD FSR 3 Frame Generation, `F1` settings menu, DualShock 4 / DualSense HID, FFT mono convolution, ahead buffer pool, static sampler ordering, and D3D12 Agility SDK matching.
- **[DexNom/VIctory](https://github.com/DexNom/VIctory)**: ray tracing fixes,
  native 1440p & FidelityFX interposition, cube gradients, sparse textures.
- **[Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD)**:
  the ambient light idea, pipeline workers, and work by romainhedouin.
- **[FQY55kai/ZJOU-VICTORY](https://github.com/FQY55kai/ZJOU-VICTORY)**.
- **[myokex/victory-amd](https://github.com/myokex/victory-amd)**: the guard over
  every view of a page (with Spexivus, joroyt and tightfall), and Joshua Odom's
  reads into title memory.
- **Geckomacabre** and **cxy0072019-sketch**: the texture read-back.
- **[XCL](https://github.com/ivall/xcl)** (GPL-2.0-only) was read for how the same
  title behaves under another layer; no code of it is here.
- This fork's changes were made with [Claude Code](https://claude.com/claude-code).

## License

GPL-3.0-or-later, as VIctory. See [LICENSE](LICENSE); the one file that is not
covered is named in [docs/asset-policy.md](docs/asset-policy.md).
