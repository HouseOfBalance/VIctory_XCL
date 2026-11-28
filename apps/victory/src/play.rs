//! `victory play`, `victory <folder-or-exe>`, and `victory` started with no
//! arguments at all (a double click): finds the title's executable from a
//! folder or saved path (or opens a Windows folder picker on first run), sets
//! `VICTORY_GAME_DIR` automatically without requiring `victory.toml`, applies
//! the play settings and anything saved by the in-game `F1` settings menu, and
//! launches the title.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The retail title's primary executable name.
const GAME: &str = "game_scarlett_master.exe";

/// Helper executable prefixes ignored when scanning a game folder.
const IGNORED_PREFIXES: &[&str] = &[
    "victory",
    "unins",
    "crash",
    "setup",
    "vc_redist",
    "dxsetup",
];

/// Subfolders skipped when scanning a parent folder for the game dump.
const IGNORED_DIRS: &[&str] = &[
    ".git",
    "target",
    "d3d12",
    "nvidia-sdk",
    "amd-sdk",
    "fidelityfx",
];

const FETCH_DLSS: &str = include_str!("../../../scripts/get-nvidia-sdks.ps1");
const FETCH_FSR: &str = include_str!("../../../scripts/get-amd-fsr.ps1");
const FETCH_D3D12: &str = include_str!("../../../scripts/get-d3d12.ps1");

const FSR_FILES: [&str; 2] = [
    "amd_fidelityfx_loader_dx12.dll",
    "amd_fidelityfx_framegeneration_dx12.dll",
];

/// Default play-profile switches (tuned for 4K 120 FPS + low latency).
const SETTINGS: &[(&str, &str)] = &[
    ("VICTORY_THIN", "1"),
    ("VICTORY_JOIN_PARTS", "8"),
    ("VICTORY_RELAY", "2"),
    ("VICTORY_RELAY_COPY", "1"),
    ("VICTORY_WIDE_BACK", "1"),
    ("VICTORY_CARD_REHASH_RUNS", "1"),
    ("VICTORY_JOIN_COPIES", "1"),
    ("VICTORY_CARD_STEPS", "2"),
    ("VICTORY_CARD_AWAKE", "1"),
    ("VICTORY_CARD_BUFFERS_MIN_KB", "64"),
    ("VICTORY_INDIRECT_WINDOW", "1"),
    ("VICTORY_INDIRECT_BATCH", "2"),
    ("VICTORY_UPLOAD_AHEAD", "1"),
    ("VICTORY_SHOW_AFTER", "1"),
    ("VICTORY_MAP_THREADS", "1"),
    ("VICTORY_TITLE_PRIORITIES", "0"),
    ("VICTORY_WC_NO_BACK", "1"),
    ("VICTORY_SOFT_VIEWS", "copies"),
    ("VICTORY_TEXTURE_NO_WAIT", "1"),
    ("VICTORY_BUFFERS_AHEAD", "4096"),
    ("VICTORY_SAMPLER_ORDER", "1"),
    ("VICTORY_FULLSCREEN", "1"),
    ("VICTORY_VSYNC", "0"),
    ("VICTORY_SKIP_BINK", "1"),
    ("VICTORY_AMBIENT", "0.8"),
    ("VICTORY_AMBIENT_AIR", "1.5"),
    ("VICTORY_AMBIENT_SPECULAR", "1"),
    ("VICTORY_EARLY_DEPTH", "1"),
    ("VICTORY_LEAVE_OUT", "02384a60fda6f2d0,731a39171b568298"),
    ("VICTORY_PIPELINE_BOOK", "1"),
    ("VICTORY_D3D12_DRED", "light"),
];

/// Conservative overrides applied when `--safe` is requested.
const SAFE_OVERRIDES: &[(&str, &str)] = &[
    ("VICTORY_THIN", "0"),
    ("VICTORY_UNDER_WIDE", "0"),
    ("VICTORY_LAND_ECHO", "0"),
    ("VICTORY_JOIN_BARRIERS", "0"),
    ("VICTORY_PORT_CONSTANTS", "0"),
    ("VICTORY_PORT_DESCRIPTORS", "0"),
    ("VICTORY_PORT_CBV", "0"),
    ("VICTORY_PORT_GUARD", "0"),
    ("VICTORY_PORT_JOURNAL", "0"),
];

/// Path variables preserved from the surrounding environment across restarts.
const KEPT: &[&str] = &[
    "VICTORY_HOME",
    "VICTORY_CONFIG",
    "VICTORY_GAME_DIR",
    "VICTORY_CACHE_DIR",
    "VICTORY_DATA_DIR",
    "VICTORY_STREAMLINE_DIR",
    "VICTORY_FIDELITYFX_DIR",
    "VICTORY_AUDIO_CAPTURE",
    "VICTORY_LOG",
];

const FG_TYPES: [&str; 3] = ["auto", "dlss", "fsr"];
const RESOLUTIONS: [&str; 3] = ["4k", "1440", "1080to1440"];
const DLSS_MODES: [&str; 6] = [
    "off",
    "dlaa",
    "quality",
    "balanced",
    "performance",
    "ultraperformance",
];
const LOG_PREFIX: &str = "victory-play-";
const WARMUP_SECONDS: u32 = 30;
const SAVED_PATH_FILE: &str = "game-path.txt";

/// Hardware tier used to auto-select the best 4K 120 FPS upscaler + Frame Gen path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GpuTier {
    /// GeForce RTX 50 series: DLSS 4 Preset K + Multi-Frame Generation 4x -> 4K 120 FPS.
    Rtx50,
    /// GeForce RTX 40 series: DLSS 4 Preset K + DLSS Frame Generation 2x -> 4K 120 FPS.
    Rtx40,
    /// GeForce RTX 20/30 / workstation RTX: DLSS 4 Preset K + AMD FSR 3 Frame Gen 2x.
    Rtx20Or30,
    /// AMD Radeon / Intel Arc / non-RTX GPUs: fast 1080p->1440p render + 4K Catmull-Rom/RCAS.
    NonNvidia,
}

pub(crate) fn classify_gpu(name: &str) -> GpuTier {
    let upper = name.to_ascii_uppercase();
    let Some(at) = upper.find("RTX") else {
        return GpuTier::NonNvidia;
    };
    let digits: String = upper[at + 3..]
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.len() == 4 && digits.starts_with("50") {
        GpuTier::Rtx50
    } else if digits.len() == 4 && digits.starts_with("40") {
        GpuTier::Rtx40
    } else {
        GpuTier::Rtx20Or30
    }
}

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct Play {
    /// Folder containing the game `.exe` (or the `.exe` itself). Remembered
    /// automatically after the first run; if omitted and not saved yet, opens
    /// a Windows folder picker dialog.
    pub(crate) path: Option<PathBuf>,
    /// Open the Windows folder picker to choose or change the game folder.
    #[arg(long)]
    pub(crate) pick: bool,
    /// Conservative mode: turns off newer renderer ports and multi-threaded
    /// thin list translation for troubleshooting.
    #[arg(long)]
    pub(crate) safe: bool,
    /// Enable the title's own ray-traced indirect lighting (`VICTORY_BVH_BUILDER=1`).
    #[arg(long)]
    pub(crate) ray_tracing: bool,
    /// Use the Series S profile (1080p output, lower VRAM/GPU load).
    #[arg(long)]
    pub(crate) series_s: bool,
    /// Series X output class: `4k` (3840x2160, default for 4K 120 FPS), `1440`
    /// (2560x1440), or `1080to1440` (1080p scene -> 1440p).
    #[arg(long, default_value = "4k", value_parser = RESOLUTIONS)]
    pub(crate) res: String,
    /// DLSS 4 Super Resolution mode (`performance` = 1080p -> 4K default;
    /// `ultraperformance` = 720p -> 4K).
    #[arg(long, default_value = "performance", value_parser = DLSS_MODES)]
    pub(crate) dlss: String,
    /// Explicit Frame Generation multiplier (`2`..`6`). When omitted, auto-picks
    /// `4` on RTX 50 and `2` on RTX 40 / RTX 20-30 (FSR 3) unless `--no-framegen`
    /// is passed.
    #[arg(long, value_parser = clap::value_parser!(u32).range(2..=6))]
    pub(crate) framegen: Option<u32>,
    /// Keep Frame Generation initialized ready at `0` (`F1` toggles it live)
    /// instead of turning it on automatically at startup.
    #[arg(long)]
    pub(crate) no_framegen: bool,
    /// Do not initialize Frame Generation at startup.
    #[arg(long)]
    pub(crate) no_framegen_ready: bool,
    /// Frame Generation backend: `auto` (DLSS FG on RTX 40/50, else AMD FSR 3),
    /// `dlss`, or `fsr`.
    #[arg(long, default_value = "auto", value_parser = FG_TYPES)]
    pub(crate) fg_type: String,
    /// Frame rate target (`120` default for 4K 120 Hz; `1000` for uncapped).
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub(crate) fps: u32,
    /// Override any `VICTORY_*` switch (`--set VICTORY_FULLSCREEN=0`).
    #[arg(long = "set", value_name = "NAME=VALUE")]
    pub(crate) more: Vec<String>,
    /// Translate all shaders and warm pipelines before launching the game.
    #[arg(long)]
    pub(crate) preload: bool,
}

/// Resolves a file or folder (searching up to two subfolder levels when needed)
/// to the title's `.exe` path.
pub(crate) fn resolve_game_exe(given: &Path) -> Result<PathBuf> {
    if given.is_file() {
        return Ok(given.to_path_buf());
    }
    anyhow::ensure!(
        given.exists(),
        "{} does not exist: give the folder containing the title's .exe, or the .exe itself",
        given.display()
    );
    anyhow::ensure!(
        given.is_dir(),
        "{} is neither a folder nor a file",
        given.display()
    );

    let top = exes_in(given)?;
    if let Some(best) = pick_best_exe(&top) {
        return Ok(best);
    }
    if top.len() > 1 {
        let names: Vec<String> = top
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        anyhow::bail!(
            "{} contains {} executables ({}): pass the title's own .exe",
            given.display(),
            top.len(),
            names.join(", ")
        );
    }

    // No candidate executable directly in `given`: check immediate subfolders
    // (depth 1, then depth 2) for dumps unpacked inside a wrapper directory.
    let mut level1 = subdirs_of(given);
    level1.sort();
    if let Some(found) = find_in_dirs(&level1)? {
        return Ok(found);
    }
    let mut level2 = Vec::new();
    for dir in &level1 {
        level2.extend(subdirs_of(dir));
    }
    level2.sort();
    if let Some(found) = find_in_dirs(&level2)? {
        return Ok(found);
    }

    anyhow::bail!(
        "no game executable found in {} (expected {GAME} or the title's .exe)",
        given.display()
    )
}

fn exes_in(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut exes: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("listing {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            if !p.is_file() || !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")) {
                return false;
            }
            let lower = p
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            !IGNORED_PREFIXES.iter().any(|prefix| lower.starts_with(prefix))
        })
        .collect();
    exes.sort();
    Ok(exes)
}

fn subdirs_of(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .is_none_or(|name| !IGNORED_DIRS.contains(&name.as_str()))
        })
        .collect()
}

fn pick_best_exe(exes: &[PathBuf]) -> Option<PathBuf> {
    if let Some(exact) = exes.iter().find(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(GAME))
    }) {
        return Some(exact.clone());
    }
    let game_prefixed: Vec<&PathBuf> = exes
        .iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with("game"))
        })
        .collect();
    if let [only] = game_prefixed.as_slice() {
        return Some((*only).clone());
    }
    if let [only] = exes {
        return Some(only.clone());
    }
    None
}

fn find_in_dirs(dirs: &[PathBuf]) -> Result<Option<PathBuf>> {
    let mut matches = Vec::new();
    for dir in dirs {
        let Ok(exes) = exes_in(dir) else { continue };
        if let Some(exact) = exes.iter().find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(GAME))
        }) {
            return Ok(Some(exact.clone()));
        }
        if let Some(best) = pick_best_exe(&exes) {
            matches.push(best);
        }
    }
    if let [only] = matches.as_slice() {
        return Ok(Some(only.clone()));
    }
    Ok(matches.into_iter().find(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with("game"))
    }))
}

/// `%LOCALAPPDATA%\victory`, or `VICTORY_HOME`.
fn home() -> Result<PathBuf> {
    if let Some(home) = victory_core::config::home_override() {
        return Ok(home);
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .context("no %LOCALAPPDATA% to store launcher settings and logs")?;
    Ok(PathBuf::from(base).join("victory"))
}

/// The game directory saved by an earlier `victory play` run, if still valid.
pub(crate) fn saved_game_dir() -> Option<PathBuf> {
    let path = home().ok()?.join(SAVED_PATH_FILE);
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'').trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = PathBuf::from(trimmed);
    if candidate.is_dir() {
        Some(candidate)
    } else if candidate.is_file() {
        candidate.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

fn save_game_dir(home_dir: &Path, game_dir: &Path) {
    let _ = std::fs::create_dir_all(home_dir);
    let _ = std::fs::write(
        home_dir.join(SAVED_PATH_FILE),
        format!("{}\r\n", game_dir.display()),
    );
}

/// Opens a native Windows folder-browser dialog (falling back to reading a path
/// from stdin when cancelled or headless).
fn pick_game_path(initial: Option<&Path>) -> Option<PathBuf> {
    const SCRIPT: &str = "\
        Add-Type -AssemblyName System.Windows.Forms; \
        $f = New-Object System.Windows.Forms.Form; \
        $f.TopMost = $true; \
        $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
        $d.Description = 'Select folder containing game .exe (Chon thu muc chua file .exe cua game)'; \
        $d.ShowNewFolderButton = $false; \
        if ($env:VICTORY_PICK_INITIAL -and (Test-Path -LiteralPath $env:VICTORY_PICK_INITIAL)) { \
            $d.SelectedPath = $env:VICTORY_PICK_INITIAL \
        }; \
        if ($d.ShowDialog($f) -eq [System.Windows.Forms.DialogResult]::OK) { \
            [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
            Write-Output $d.SelectedPath \
        }";
    println!("Opening folder picker: select the folder containing your game .exe ...");
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-Sta", "-ExecutionPolicy", "Bypass", "-Command", SCRIPT]);
    if let Some(dir) = initial {
        cmd.env("VICTORY_PICK_INITIAL", dir);
    }
    if let Ok(out) = cmd.output() {
        let picked = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !picked.is_empty() {
            return Some(PathBuf::from(picked));
        }
    }
    if std::io::stdin().is_terminal() {
        print!("Paste the path to your game folder (or .exe), or press Enter to cancel: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let trimmed = line.trim().trim_matches('"').trim_matches('\'').trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    None
}

fn locate_exe(o: &Play, home_dir: &Path, here: &Path) -> Result<PathBuf> {
    let previous = saved_game_dir();
    if o.pick {
        let chosen = pick_game_path(previous.as_deref())
            .context("no game folder selected")?;
        return resolve_game_exe(&chosen);
    }
    if let Some(given) = &o.path {
        return resolve_game_exe(given);
    }
    for local in [here.join(GAME), PathBuf::from(GAME)] {
        if local.is_file() {
            return Ok(local);
        }
    }
    if let Some(saved) = &previous
        && let Ok(exe) = resolve_game_exe(saved)
    {
        return Ok(exe);
    }
    if let Ok(cfg) = victory_core::Config::load()
        && let Some(game_dir) = &cfg.game_dir
        && let Ok(exe) = resolve_game_exe(game_dir)
    {
        return Ok(exe);
    }
    if let Some(chosen) = pick_game_path(previous.as_deref()) {
        return resolve_game_exe(&chosen);
    }
    anyhow::bail!(
        "no game folder configured yet (saved path: {}). Run `victory play <path-to-game-folder>` \
         or double-click Launch-VIctory.cmd to select the folder containing {GAME}",
        home_dir.join(SAVED_PATH_FILE).display()
    )
}

pub(crate) fn explain_preload() {
    println!();
    println!("Preloading shaders and known pipelines for your game folder.");
    println!("Progress is saved continuously: if interrupted, the next preload resumes where it stopped.");
    println!();
}

pub(crate) fn preload(exe: &Path) {
    let folder = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let began = std::time::Instant::now();
    let done = std::env::current_exe().and_then(|program| {
        std::process::Command::new(program)
            .arg("preload")
            .arg(&folder)
            .env_remove("VICTORY_LOG_FILE")
            .status()
    });
    let minutes = began.elapsed().as_secs_f64() / 60.0;
    match done {
        Ok(s) if s.success() => println!("Shaders preloaded in {minutes:.1} min. Starting the game."),
        Ok(s) => println!("Preload stopped ({s}) after {minutes:.1} min; progress is kept. Starting the game."),
        Err(e) => println!("Preload could not start ({e}). Starting the game."),
    }
}

fn nvidia_home() -> Result<PathBuf> {
    Ok(home()?.join("nvidia-sdk"))
}

fn has_streamline(dir: &Path) -> bool {
    dir.join("sl.interposer.dll").is_file()
}

fn streamline_dir(here: &Path) -> Option<PathBuf> {
    let named = std::env::var_os("VICTORY_STREAMLINE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let ours = nvidia_home()
        .ok()
        .map(|d| d.join("Streamline").join("bin").join("x64"));
    let beside = [
        here.join("nvidia-sdk").join("Streamline").join("bin").join("x64"),
        here.to_path_buf(),
    ];
    named.into_iter().chain(ours).chain(beside).find(|d| has_streamline(d))
}

fn amd_home() -> Result<PathBuf> {
    Ok(home()?.join("amd-fidelityfx"))
}

fn has_fsr(dir: &Path) -> bool {
    FSR_FILES.iter().all(|f| dir.join(f).is_file())
}

fn fidelityfx_dir(here: &Path) -> Option<PathBuf> {
    let named = std::env::var_os("VICTORY_FIDELITYFX_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let ours = amd_home().ok();
    let beside = [
        here.join("fidelityfx"),
        here.join("amd-sdk").join("FidelityFX").join("bin"),
        here.to_path_buf(),
    ];
    named.into_iter().chain(ours).chain(beside).find(|d| has_fsr(d))
}

fn parse_menu(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .filter(|(k, _)| {
            k.starts_with("VICTORY_")
                && k.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
        .collect()
}

fn settings(
    o: &Play,
    menu: &[(String, String)],
    streamline: Option<&Path>,
    tier: GpuTier,
) -> (BTreeMap<String, String>, bool) {
    let saved = |k: &str| menu.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let series_s = saved("VICTORY_MENU_CONSOLE").map_or(o.series_s, |c| c == "series-s");
    let res = saved("VICTORY_MENU_RES")
        .filter(|r| RESOLUTIONS.contains(r))
        .unwrap_or(&o.res);
    let dlss_explicit = saved("VICTORY_MENU_DLSS").filter(|d| DLSS_MODES.contains(d));
    let dlss = dlss_explicit.unwrap_or(&o.dlss);
    let auto_fg = match tier {
        GpuTier::Rtx50 => Some(4),
        GpuTier::Rtx40 | GpuTier::Rtx20Or30 => Some(2),
        GpuTier::NonNvidia => None,
    };
    let default_fg = if o.no_framegen || o.no_framegen_ready {
        None
    } else {
        o.framegen.or(auto_fg)
    };
    let (mut framegen, mut times) = (default_fg.is_some(), default_fg.unwrap_or(2));
    if let Some(saved_fg) = saved("VICTORY_MENU_FRAMEGEN") {
        let n = saved_fg.parse::<u32>().ok().filter(|n| (1..=6).contains(n));
        framegen = n.is_some();
        if let Some(n) = n
            && n >= 2
        {
            times = n;
        }
    }
    let fg_type = saved("VICTORY_MENU_FG_TYPE")
        .filter(|t| FG_TYPES.contains(t))
        .unwrap_or(&o.fg_type);
    let fps = saved("VICTORY_MENU_FPS")
        .and_then(|f| f.parse::<u32>().ok())
        .filter(|f| (1..=1000).contains(f))
        .unwrap_or(o.fps);

    let mut s: BTreeMap<String, String> = SETTINGS
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    if o.safe {
        s.remove("VICTORY_JOIN_PARTS");
        s.remove("VICTORY_UPLOAD_AHEAD");
        for (k, v) in SAFE_OVERRIDES {
            s.insert((*k).to_owned(), (*v).to_owned());
        }
    }
    if o.ray_tracing {
        s.insert("VICTORY_BVH_BUILDER".to_owned(), "1".to_owned());
    }

    let (mut dlss_on, mut framegen_x) = (false, None);
    if dlss != "off"
        && let Some(dir) = streamline
    {
        dlss_on = true;
        s.insert("VICTORY_DLSS".to_owned(), "1".to_owned());
        s.insert("VICTORY_DLSS_MODE".to_owned(), dlss.to_owned());
        // DLSS 4 Transformer model (Preset K) for crisp 4K Performance/UltraPerformance.
        s.insert("VICTORY_DLSS_PRESET".to_owned(), "k".to_owned());
        s.insert(
            "VICTORY_STREAMLINE_DIR".to_owned(),
            dir.to_string_lossy().into_owned(),
        );
        if framegen {
            framegen_x = Some(times);
        } else if !o.no_framegen_ready {
            framegen_x = Some(0);
        }
        if let Some(x) = framegen_x {
            s.insert("VICTORY_FRAMEGEN".to_owned(), "1".to_owned());
            s.insert("VICTORY_FRAMEGEN_X".to_owned(), x.to_string());
            s.insert(
                "VICTORY_TEXTURE_ROOM_PERCENT".to_owned(),
                if framegen { "80" } else { "86" }.to_owned(),
            );
        }
    }

    if !series_s {
        match res {
            "1440" => {
                s.insert("VICTORY_NATIVE_1440P".to_owned(), "1".to_owned());
            }
            "1080to1440" => {
                s.insert("VICTORY_RENDER_1080P".to_owned(), "1".to_owned());
            }
            "4k" if !dlss_on && dlss != "off" => {
                // Non-DLSS GPUs (AMD Radeon / Intel Arc) aiming for 4K 120 Hz:
                // render scene fast at 1080p->1440p and upscale to 3840x2160 via
                // D3D12 5-tap Catmull-Rom + RCAS spatial upscaler.
                s.insert("VICTORY_RENDER_1080P".to_owned(), "1".to_owned());
                s.insert("VICTORY_OUTPUT_RES".to_owned(), "3840x2160".to_owned());
                s.insert("VICTORY_HOST_UPSCALER".to_owned(), "cas".to_owned());
            }
            _ => {}
        }
    }

    if fg_type != "dlss" && (framegen || !o.no_framegen_ready) && (dlss_on || fg_type == "fsr") {
        s.insert("VICTORY_FSR_FRAMEGEN".to_owned(), "1".to_owned());
        if fg_type == "fsr" {
            s.remove("VICTORY_FRAMEGEN");
        }
        let x = *framegen_x.get_or_insert(if framegen { times } else { 0 });
        s.insert("VICTORY_FRAMEGEN_X".to_owned(), x.to_string());
        if !s.contains_key("VICTORY_TEXTURE_ROOM_PERCENT") {
            s.insert(
                "VICTORY_TEXTURE_ROOM_PERCENT".to_owned(),
                if framegen { "80" } else { "86" }.to_owned(),
            );
        }
    }
    s.insert(
        "VICTORY_TARGET_FPS".to_owned(),
        if fps >= 1000 {
            "0".to_owned()
        } else {
            fps.to_string()
        },
    );
    for (k, v) in menu.iter().filter(|(k, _)| !k.starts_with("VICTORY_MENU_")) {
        if v.is_empty() {
            s.remove(k);
        } else {
            s.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in o.more.iter().filter_map(|m| m.split_once('=')) {
        s.insert(k.trim().to_owned(), v.trim().to_owned());
    }
    s.insert(
        "VICTORY_MENU_CONSOLE".to_owned(),
        if series_s { "series-s" } else { "series-x" }.to_owned(),
    );
    s.insert("VICTORY_MENU_RES".to_owned(), res.to_owned());
    s.insert(
        "VICTORY_MENU_DLSS".to_owned(),
        if dlss_on { dlss } else { "off" }.to_owned(),
    );
    s.insert(
        "VICTORY_MENU_FRAMEGEN".to_owned(),
        framegen_x.unwrap_or(0).to_string(),
    );
    s.insert("VICTORY_MENU_FG_TYPE".to_owned(), fg_type.to_owned());
    s.insert("VICTORY_MENU_FPS".to_owned(), fps.to_string());
    (s, series_s)
}

fn stamp(seconds: u64) -> String {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (day, month) = (
        doy - (153 * mp + 2) / 5 + 1,
        if mp < 10 { mp + 3 } else { mp - 9 },
    );
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn log_file(logs: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(logs).with_context(|| format!("creating {}", logs.display()))?;
    let mut old: Vec<PathBuf> = std::fs::read_dir(logs)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(LOG_PREFIX))
        })
        .collect();
    old.sort();
    for gone in old.iter().rev().skip(9) {
        let _ = std::fs::remove_file(gone);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Ok(logs.join(format!("{LOG_PREFIX}{}-{}.log", stamp(now), std::process::id())))
}

fn fetch_dlss(to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    let script = to.join("get-nvidia-sdks.ps1");
    std::fs::write(&script, FETCH_DLSS).with_context(|| format!("writing {}", script.display()))?;
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-To")
        .arg(to)
        .status()
        .context("starting Windows PowerShell")?;
    let _ = std::fs::remove_file(&script);
    anyhow::ensure!(
        status.success() && has_streamline(&to.join("Streamline").join("bin").join("x64")),
        "NVIDIA Streamline files did not arrive; running without DLSS"
    );
    Ok(())
}

pub(crate) fn get_dlss() -> Result<()> {
    let to = nvidia_home()?;
    let _ = std::fs::remove_file(to.join("declined"));
    fetch_dlss(&to)
}

fn offer_dlss() {
    let Ok(to) = nvidia_home() else { return };
    let declined = to.join("declined");
    if declined.exists() || !std::io::stdin().is_terminal() {
        println!("No NVIDIA Streamline DLLs found: running without DLSS (`victory get-dlss` fetches them).");
        return;
    }
    println!("NVIDIA Streamline / DLSS DLLs are not on this PC yet (~70 MB from github.com/NVIDIA-RTX/Streamline).");
    print!("Download them now? [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    if answer.trim().to_ascii_lowercase().starts_with('n') {
        let _ = std::fs::create_dir_all(&to);
        let _ = std::fs::write(&declined, "asked once; `victory get-dlss` fetches them\r\n");
        println!("Skipped (`victory get-dlss` downloads them later).");
    } else if let Err(e) = fetch_dlss(&to) {
        println!("{e:#}");
    }
}

fn fetch_fsr(to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    let script = to.join("get-amd-fsr.ps1");
    std::fs::write(&script, FETCH_FSR).with_context(|| format!("writing {}", script.display()))?;
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-To")
        .arg(to)
        .status()
        .context("starting Windows PowerShell")?;
    let _ = std::fs::remove_file(&script);
    anyhow::ensure!(
        status.success() && has_fsr(to),
        "AMD FidelityFX files did not arrive; running without FSR 3 Frame Generation"
    );
    Ok(())
}

pub(crate) fn get_fsr() -> Result<()> {
    let to = amd_home()?;
    let _ = std::fs::remove_file(to.join("declined"));
    fetch_fsr(&to)
}

fn offer_fsr(card: &str) {
    let Ok(to) = amd_home() else { return };
    let declined = to.join("declined");
    if declined.exists() || !std::io::stdin().is_terminal() {
        println!("No AMD FSR 3 DLLs found: Frame Generation stays off (`victory get-fsr` fetches them).");
        return;
    }
    if card.is_empty() {
        println!("AMD FSR 3 Frame Generation is selected, and AMD's signed DLLs are not on this PC yet.");
    } else {
        println!("GPU ({card}) can use AMD FSR 3 2x Frame Generation (~30 MB from GPUOpen FidelityFX-SDK).");
    }
    print!("Download AMD FSR 3 DLLs now? [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    if answer.trim().to_ascii_lowercase().starts_with('n') {
        let _ = std::fs::create_dir_all(&to);
        let _ = std::fs::write(&declined, "asked once; `victory get-fsr` fetches them\r\n");
        println!("Skipped (`victory get-fsr` downloads them later).");
    } else if let Err(e) = fetch_fsr(&to) {
        println!("{e:#}");
    }
}

fn d3d12_dir() -> Result<PathBuf> {
    let program = std::env::current_exe().context("this program's own path")?;
    Ok(program
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join("D3D12"))
}

fn shader_model() -> Option<(u32, u32)> {
    let program = std::env::current_exe().ok()?;
    let out = std::process::Command::new(program)
        .arg("shader-model")
        .env_remove("VICTORY_LOG_FILE")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (major, minor) = text.lines().rev().find_map(|l| l.trim().split_once('.'))?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn fetch_d3d12(to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)
        .with_context(|| format!("creating {} (beside victory.exe)", to.display()))?;
    let script = std::env::temp_dir().join(format!("victory-get-d3d12-{}.ps1", std::process::id()));
    std::fs::write(&script, FETCH_D3D12).with_context(|| format!("writing {}", script.display()))?;
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-To")
        .arg(to)
        .status()
        .context("starting Windows PowerShell")?;
    let _ = std::fs::remove_file(&script);
    anyhow::ensure!(
        status.success() && to.join("D3D12Core.dll").is_file(),
        "Microsoft's Direct3D 12 runtime did not arrive"
    );
    Ok(())
}

pub(crate) fn get_d3d12() -> Result<()> {
    fetch_d3d12(&d3d12_dir()?)
}

fn check_shader_model() -> Result<()> {
    let needed = victory_hle::streamline::SHADER_MODEL_NEEDED;
    let Some(found) = shader_model() else {
        return Ok(());
    };
    let to = d3d12_dir()?;
    let had = to.join("D3D12Core.dll").is_file();
    let windows_10 = !had && crate::windows_build().is_some_and(|build| build < 22000);
    if found >= needed && !windows_10 {
        return Ok(());
    }
    if !had {
        if found < needed {
            println!(
                "System Direct3D 12 reports Shader Model {}.{} (6.6 required). Microsoft's Agility SDK provides 6.6.",
                found.0, found.1
            );
        } else {
            println!(
                "Windows 10 detected: Microsoft's D3D12 Agility SDK (~22 MB from nuget.org) is needed for SM 6.6 root signatures."
            );
        }
        let yes = if std::io::stdin().is_terminal() {
            print!("Download Microsoft D3D12 Agility SDK into {} now? [Y/n] ", to.display());
            let _ = std::io::stdout().flush();
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            !answer.trim().to_ascii_lowercase().starts_with('n')
        } else {
            true
        };
        if yes {
            match fetch_d3d12(&to) {
                Ok(()) => {
                    crate::match_d3d12_core();
                    if let Some(now) = shader_model()
                        && now >= needed
                    {
                        println!("Shader Model {}.{} ready. Starting the game.", now.0, now.1);
                        return Ok(());
                    }
                }
                Err(e) => println!("{e:#}"),
            }
        }
    }
    if found >= needed {
        println!("Continuing with built-in Direct3D 12 (`victory get-d3d12` fetches the Agility SDK later).");
        return Ok(());
    }
    let now = shader_model().unwrap_or(found);
    anyhow::bail!(
        "Direct3D 12 reports Shader Model {}.{}, below required 6.6. Run `victory get-d3d12` or update your GPU driver.",
        now.0,
        now.1
    )
}

fn warmup_mark(home_dir: &Path, program: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(program).ok()?;
    let hash = blake3::hash(&bytes).to_hex();
    Some(home_dir.join("warmed").join(&hash.as_str()[..16]))
}

pub(crate) struct Ready {
    pub(crate) exe: PathBuf,
    pub(crate) series_s: bool,
}

pub(crate) fn prepare(o: &Play) -> Result<Ready> {
    let program = std::env::current_exe().context("this program's own path")?;
    let here = program.parent().map(Path::to_path_buf).unwrap_or_default();
    let home_dir = home()?;
    std::fs::create_dir_all(&home_dir)
        .with_context(|| format!("creating {}", home_dir.display()))?;

    let exe = locate_exe(o, &home_dir, &here)?;
    anyhow::ensure!(exe.is_file(), "no {}", exe.display());
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let exe = PathBuf::from(exe.to_string_lossy().trim_start_matches(r"\\?\").to_owned());
    let game_dir = exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    victory_core::ensure_outside_repo(&game_dir)?;
    save_game_dir(&home_dir, &game_dir);
    println!("Game exe : {}", exe.display());

    check_shader_model()?;

    let menu_file = home_dir.join("victory-settings.txt");
    let menu = parse_menu(&std::fs::read_to_string(&menu_file).unwrap_or_default());
    let card = victory_hle::streamline::adapter_name().unwrap_or_default();
    let tier = classify_gpu(&card);
    let is_rtx = tier != GpuTier::NonNvidia;
    let explicit_dlss = menu
        .iter()
        .rev()
        .find(|(k, _)| k == "VICTORY_MENU_DLSS")
        .map(|(_, v)| v.as_str());
    let wants_dlss = explicit_dlss.unwrap_or(o.dlss.as_str()) != "off"
        && (is_rtx || explicit_dlss.is_some());
    if wants_dlss && streamline_dir(&here).is_none() {
        offer_dlss();
    }
    let streamline = streamline_dir(&here);
    let (mut s, series_s) = settings(o, &menu, streamline.as_deref(), tier);
    if !card.is_empty() {
        println!(
            "GPU      : {card} (4K 120 Hz profile: res={}, dlss={}, fg={}x, target={} fps)",
            s.get("VICTORY_MENU_RES").map_or("4k", String::as_str),
            s.get("VICTORY_MENU_DLSS").map_or("off", String::as_str),
            s.get("VICTORY_MENU_FRAMEGEN").map_or("0", String::as_str),
            s.get("VICTORY_MENU_FPS").map_or("120", String::as_str),
        );
    }

    if std::env::var_os("VICTORY_GAME_DIR").is_none_or(|v| v.is_empty()) {
        s.insert(
            "VICTORY_GAME_DIR".to_owned(),
            game_dir.to_string_lossy().into_owned(),
        );
    }

    if s.contains_key("VICTORY_FSR_FRAMEGEN") {
        if fidelityfx_dir(&here).is_none() {
            let chosen = s.get("VICTORY_MENU_FG_TYPE").is_some_and(|t| t == "fsr");
            if chosen || (!card.is_empty() && !victory_hle::streamline::dlss_framegen_card(&card)) {
                offer_fsr(if chosen { "" } else { &card });
            }
        }
        if let Some(dir) = fidelityfx_dir(&here) {
            s.insert(
                "VICTORY_FIDELITYFX_DIR".to_owned(),
                dir.to_string_lossy().into_owned(),
            );
        }
    }

    if !s.contains_key("VICTORY_WARMUP_RESTART")
        && let Some(mark) = warmup_mark(&home_dir, &program)
        && !mark.exists()
    {
        println!(
            "First run of this victory.exe build: the game will warm up for {WARMUP_SECONDS}s in-scene and restart once automatically."
        );
        s.insert("VICTORY_WARMUP_RESTART".to_owned(), WARMUP_SECONDS.to_string());
        s.insert(
            "VICTORY_WARMUP_MARK".to_owned(),
            mark.to_string_lossy().into_owned(),
        );
    }

    let quoted = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "''"));
    s.insert(
        "VICTORY_MENU_FILE".to_owned(),
        menu_file.to_string_lossy().into_owned(),
    );
    s.insert(
        "VICTORY_MENU_PLAY".to_owned(),
        program.to_string_lossy().into_owned(),
    );
    s.insert(
        "VICTORY_MENU_ARGS".to_owned(),
        format!(
            "play {}{}{}{}{}",
            quoted(&exe),
            if o.safe { " --safe" } else { "" },
            if o.ray_tracing { " --ray-tracing" } else { "" },
            if o.no_framegen { " --no-framegen" } else { "" },
            if o.no_framegen_ready {
                " --no-framegen-ready"
            } else {
                ""
            }
        ),
    );

    let log = log_file(&home_dir.join("logs"))?;
    let console = if series_s { "series-s" } else { "series-x" };
    let line: Vec<String> = s.iter().map(|(k, v)| format!("{k}={v}")).collect();
    std::fs::write(&log, format!("# console={console} switches: {}\n", line.join(" ")))
        .with_context(|| format!("writing {}", log.display()))?;
    println!("Log file : {}", log.display());
    println!("Controls : F1 = Settings Menu | F3 = Stats Panel | F11 / Alt+Enter = Fullscreen");
    s.insert(
        "VICTORY_LOG_FILE".to_owned(),
        log.to_string_lossy().into_owned(),
    );

    let leftover: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| k.starts_with("VICTORY_") && !KEPT.contains(&k.as_str()))
        .collect();
    // SAFETY: called at process start before worker threads are spawned.
    unsafe {
        for k in leftover {
            std::env::remove_var(k);
        }
        for (k, v) in &s {
            std::env::set_var(k, v);
        }
    }
    Ok(Ready { exe, series_s })
}

pub(crate) fn hold_window() {
    if std::io::stdin().is_terminal() {
        print!("Press Enter to close this window.");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Play {
        Play {
            path: None,
            pick: false,
            safe: false,
            ray_tracing: false,
            series_s: false,
            res: "4k".to_owned(),
            dlss: "performance".to_owned(),
            framegen: None,
            no_framegen: false,
            no_framegen_ready: false,
            fg_type: "auto".to_owned(),
            fps: 120,
            more: Vec::new(),
            preload: false,
        }
    }

    #[test]
    fn classifies_gpu_tiers_for_4k_120_auto_tuning() {
        assert_eq!(classify_gpu("NVIDIA GeForce RTX 5090"), GpuTier::Rtx50);
        assert_eq!(classify_gpu("NVIDIA GeForce RTX 5070 Ti"), GpuTier::Rtx50);
        assert_eq!(classify_gpu("NVIDIA GeForce RTX 4080 SUPER"), GpuTier::Rtx40);
        assert_eq!(classify_gpu("NVIDIA GeForce RTX 3080"), GpuTier::Rtx20Or30);
        assert_eq!(classify_gpu("NVIDIA GeForce RTX 2060"), GpuTier::Rtx20Or30);
        assert_eq!(classify_gpu("NVIDIA GeForce GTX 1080 Ti"), GpuTier::NonNvidia);
        assert_eq!(classify_gpu("AMD Radeon RX 7900 XTX"), GpuTier::NonNvidia);
        assert_eq!(classify_gpu("Intel(R) UHD Graphics 620"), GpuTier::NonNvidia);
    }

    #[test]
    fn resolves_game_exe_from_file_folder_and_nested_dump_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("DumpRoot").join("Content");
        std::fs::create_dir_all(&nested).unwrap();
        let helper = nested.join("unins000.exe");
        let game = nested.join(GAME);
        std::fs::write(&helper, b"stub").unwrap();
        std::fs::write(&game, b"stub").unwrap();

        // Direct .exe path
        assert_eq!(resolve_game_exe(&game).unwrap(), game);
        // Folder directly containing the .exe (plus uninstaller helper)
        assert_eq!(resolve_game_exe(&nested).unwrap(), game);
        // Parent folder two levels above Content\game_scarlett_master.exe
        assert_eq!(resolve_game_exe(tmp.path()).unwrap(), game);
    }

    #[test]
    fn empty_folder_reports_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_game_exe(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("no game executable found"), "{err}");
    }

    #[test]
    fn rtx50_and_rtx40_auto_configure_4k_120_dlss4_and_framegen() {
        let dir = Path::new(r"C:\nvidia");
        let (s50, series_s) = settings(&options(), &[], Some(dir), GpuTier::Rtx50);
        assert!(!series_s);
        for (k, v) in SETTINGS {
            assert_eq!(s50.get(*k).map(String::as_str), Some(*v), "{k}");
        }
        let get50 = |k: &str| s50.get(k).map(String::as_str);
        assert_eq!(get50("VICTORY_NATIVE_1440P"), None);
        assert_eq!(
            (get50("VICTORY_DLSS"), get50("VICTORY_DLSS_MODE"), get50("VICTORY_DLSS_PRESET")),
            (Some("1"), Some("performance"), Some("k"))
        );
        assert_eq!(
            (get50("VICTORY_FRAMEGEN"), get50("VICTORY_FRAMEGEN_X")),
            (Some("1"), Some("4"))
        );
        assert_eq!(get50("VICTORY_TEXTURE_ROOM_PERCENT"), Some("80"));
        assert_eq!(get50("VICTORY_TARGET_FPS"), Some("120"));
        assert_eq!(get50("VICTORY_MENU_RES"), Some("4k"));
        assert_eq!(get50("VICTORY_MENU_FRAMEGEN"), Some("4"));

        let (s40, _) = settings(&options(), &[], Some(dir), GpuTier::Rtx40);
        assert_eq!(s40.get("VICTORY_FRAMEGEN_X").map(String::as_str), Some("2"));
    }

    #[test]
    fn amd_and_non_dlss_gpus_auto_configure_fast_4k_rcas_upscaling() {
        let (s, _) = settings(&options(), &[], None, GpuTier::NonNvidia);
        let get = |k: &str| s.get(k).map(String::as_str);
        assert_eq!(get("VICTORY_RENDER_1080P"), Some("1"));
        assert_eq!(get("VICTORY_OUTPUT_RES"), Some("3840x2160"));
        assert_eq!(get("VICTORY_HOST_UPSCALER"), Some("cas"));
        assert_eq!(get("VICTORY_TARGET_FPS"), Some("120"));
        assert_eq!(get("VICTORY_DLSS"), None);
    }

    #[test]
    fn safe_and_ray_tracing_options_override_defaults() {
        let mut o = options();
        o.safe = true;
        o.ray_tracing = true;
        o.fps = 60;
        let (s, _) = settings(&o, &[], None, GpuTier::NonNvidia);
        let get = |k: &str| s.get(k).map(String::as_str);
        assert_eq!(get("VICTORY_THIN"), Some("0"));
        assert_eq!(get("VICTORY_PORT_CONSTANTS"), Some("0"));
        assert_eq!(get("VICTORY_BVH_BUILDER"), Some("1"));
        assert_eq!(get("VICTORY_TARGET_FPS"), Some("60"));
        assert_eq!(get("VICTORY_JOIN_PARTS"), None);
    }

    #[test]
    fn menu_saved_choices_override_cli_defaults() {
        let menu = parse_menu(
            "# Saved by F1\r\nVICTORY_MENU_FRAMEGEN=3\r\nVICTORY_MENU_CONSOLE=series-s\r\n\
             VICTORY_MENU_DLSS=ultraperformance\r\nVICTORY_MENU_FPS=1000\r\nVICTORY_AMBIENT=0.5\r\n\
             VICTORY_EARLY_DEPTH=\r\n",
        );
        let (s, series_s) = settings(&options(), &menu, Some(Path::new("x")), GpuTier::Rtx40);
        let get = |k: &str| s.get(k).map(String::as_str);
        assert!(series_s);
        assert_eq!(get("VICTORY_NATIVE_1440P"), None);
        assert_eq!(
            (get("VICTORY_DLSS_MODE"), get("VICTORY_FRAMEGEN_X")),
            (Some("ultraperformance"), Some("3"))
        );
        assert_eq!(get("VICTORY_TARGET_FPS"), Some("0"));
        assert_eq!((get("VICTORY_AMBIENT"), get("VICTORY_EARLY_DEPTH")), (Some("0.5"), None));
    }

    #[test]
    fn stamps_are_dates() {
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(1_795_753_200), "20261127-042000");
        assert_eq!(stamp(951_782_400), "20000229-000000");
    }
}
