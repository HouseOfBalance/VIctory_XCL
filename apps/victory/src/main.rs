//! `victory`: the command-line front end.
//!
//! Every command reads the user's own files and writes only to stdout or to
//! the user's cache directory (outside the repository).

mod census;
mod inspect;
#[cfg(all(windows, feature = "diagnostics"))]
mod replay;
mod run;
mod shader;
mod translate;
#[cfg(windows)]
mod warm;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

/// Microsoft D3D12 Agility SDK 1.614 (`D3D12\D3D12Core.dll` beside `victory.exe`
/// when present; matched dynamically via [`match_d3d12_core`] before delay-loaded
/// `d3d12.dll` initializes).
#[cfg(windows)]
#[unsafe(no_mangle)]
#[used]
#[allow(non_upper_case_globals)]
pub static D3D12SDKVersion: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(614);

#[cfg(any(windows, test))]
fn agility_version(dll: &[u8]) -> Option<u32> {
    let signature = 0xFEEF_04BD_u32.to_le_bytes();
    let at = dll.windows(4).position(|w| w == signature)?;
    let most = u32::from_le_bytes(dll.get(at + 8..at + 12)?.try_into().ok()?);
    let (major, minor) = (most >> 16, most & 0xffff);
    (major == 1 && minor >= 600).then_some(minor)
}

#[cfg(any(windows, test))]
fn exported_sdk_version(dll: &[u8]) -> Option<u32> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(dll.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(dll.get(at..at + 4)?.try_into().ok()?));
    let pe = u32_at(0x3c)? as usize;
    (dll.get(pe..pe + 4)? == b"PE\0\0").then_some(())?;
    let sections = usize::from(u16_at(pe + 6)?);
    let optional = pe + 24;
    let table = optional + usize::from(u16_at(pe + 20)?);
    (u16_at(optional)? == 0x20b).then_some(())?;
    let offset = |rva: u32| -> Option<usize> {
        (0..sections).find_map(|i| {
            let s = table + i * 40;
            let (size, va, raw_size, raw) = (u32_at(s + 8)?, u32_at(s + 12)?, u32_at(s + 16)?, u32_at(s + 20)?);
            (va <= rva && rva < va + size.max(raw_size)).then(|| (rva - va + raw) as usize)
        })
    };
    let exports = offset(u32_at(optional + 112)?)?;
    let count = u32_at(exports + 24)? as usize;
    let functions_at = offset(u32_at(exports + 28)?)?;
    let names_at = offset(u32_at(exports + 32)?)?;
    let ordinals_at = offset(u32_at(exports + 36)?)?;
    (0..count).find_map(|i| {
        let name = offset(u32_at(names_at + 4 * i)?)?;
        if dll.get(name..name + 16)? != b"D3D12SDKVersion\0" {
            return None;
        }
        let ordinal = usize::from(u16_at(ordinals_at + 2 * i)?);
        u32_at(offset(u32_at(functions_at + 4 * ordinal)?)?)
    })
}

#[cfg(windows)]
fn os_runtime() -> u32 {
    let system = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| r"C:\Windows".into());
    std::fs::read(system.join("System32").join("D3D12Core.dll"))
        .ok()
        .and_then(|dll| exported_sdk_version(&dll))
        .map_or(4, |v| v.min(614))
}

#[cfg(windows)]
pub(crate) fn match_d3d12_core() {
    use std::sync::atomic::Ordering::Relaxed;
    let Some(core) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("D3D12").join("D3D12Core.dll")))
    else {
        return;
    };
    let Ok(bytes) = std::fs::read(&core) else {
        D3D12SDKVersion.store(os_runtime(), Relaxed);
        return;
    };
    match agility_version(&bytes) {
        Some(v) => {
            if v != 614 {
                println!("{} is the D3D12 Agility SDK 1.{v}: asking Windows for that version.", core.display());
            }
            D3D12SDKVersion.store(v, Relaxed);
        }
        None => {
            D3D12SDKVersion.store(os_runtime(), Relaxed);
            println!("{} is not a D3D12 Agility SDK runtime Windows can use: Windows' own is used.", core.display());
        }
    }
}

#[cfg(windows)]
#[repr(transparent)]
pub struct SdkPath(*const std::ffi::c_char);
// SAFETY: points at a static C string literal, never written.
#[cfg(windows)]
unsafe impl Sync for SdkPath {}

#[cfg(windows)]
#[unsafe(no_mangle)]
#[used]
#[allow(non_upper_case_globals)]
pub static D3D12SDKPath: SdkPath = SdkPath(c".\\D3D12\\".as_ptr());

#[derive(Parser)]
#[command(
    name = "victory",
    version,
    about = "Run an Xbox Series title natively on Windows"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Summarise one PE image: headers, sections, imports, TLS, build id.
    Inspect {
        file: PathBuf,
        /// List every imported function.
        #[arg(long)]
        imports: bool,
        /// List every exported function.
        #[arg(long)]
        exports: bool,
    },
    /// Aggregate the imports of every PE image under the given paths and
    /// show which ones our runtime covers. Defaults to the configured
    /// game and test-title directories.
    Census {
        paths: Vec<PathBuf>,
        /// Do not count host pass-through as covered.
        #[arg(long)]
        strict: bool,
        /// List every missing `dll!function`, most used first.
        #[arg(long)]
        missing: bool,
    },
    /// List the `dll!export` pairs an import library describes.
    LibExports {
        lib: PathBuf,
        /// List every export, not just per-DLL counts.
        #[arg(long)]
        list: bool,
    },
    /// Find shader containers in files or directories (also inside engine
    /// archives, if uncompressed) and report their layout.
    ShaderInfo {
        paths: Vec<PathBuf>,
        /// Print one line per container found and show why paths were skipped.
        #[arg(short, long)]
        verbose: bool,
        /// Also list the decoded native instructions (implies --verbose).
        #[arg(long)]
        disasm: bool,
    },
    /// Run every shader found (containers, SGD5 files) through the
    /// translator and report what it cannot handle yet.
    ///
    /// VICTORY_HOST_WAVE overrides the host wave width, including with --warm.
    /// VICTORY_HOST_MAX_WAVE is a compatibility alias. When both values are
    /// valid, VICTORY_HOST_WAVE takes precedence. Zero means unknown width.
    /// For ordinary translation, VICTORY_HOST_RDNA=1 selects RDNA float settings.
    Translate {
        paths: Vec<PathBuf>,
        /// Check emitted DXIL with dxil.dll's validator (`VICTORY_DXIL`,
        /// else the newest Windows SDK's).
        #[arg(long)]
        validate: bool,
        /// Also create a host D3D12 pipeline for each emitted shader.
        #[arg(long)]
        pipelines: bool,
        /// Print one line per program and outcome.
        #[arg(long)]
        list: bool,
        /// Worker threads (default: every logical core).
        #[arg(long)]
        jobs: Option<usize>,
        /// Warm the run's shader cache: a file of shader hashes (the 16
        /// hex digits a run's log names a shader by, anywhere in a line);
        /// the containers with those hashes are translated into the
        /// cache and nothing else is done. Set `VICTORY_HOST_WAVE` and
        /// `VICTORY_VIEW_LIMIT` to what the run's log says of the host.
        #[arg(long)]
        warm: Option<PathBuf>,
    },
    /// Translate every shader found (as `translate` finds them) into the
    /// shader cache, so that no run has to: a few minutes, once per version
    /// of the translator. Translations an earlier build of the same version
    /// cached are made again, and those that came out different counted.
    /// In diagnostics builds, VICTORY_LOG=debug shows skipped paths.
    Shaders {
        paths: Vec<PathBuf>,
        /// Worker threads (default: every logical core).
        #[arg(long)]
        jobs: Option<usize>,
        /// Also write every shader that does not translate to this file,
        /// one line each: hash, where it was found, why.
        #[arg(long)]
        list: Option<PathBuf>,
    },
    /// Make the host pipelines of every pipeline the title ever created
    /// (runs with `VICTORY_PIPELINE_BOOK=1` write them down), so that the
    /// driver's cache on disk has them before a run needs one: once per change
    /// of the translator or the driver. Goes on where it was stopped.
    #[cfg(windows)]
    Warm {
        /// Processes at once (default: two thirds of the logical cores).
        #[arg(long)]
        jobs: Option<usize>,
        /// At most this many pipelines.
        #[arg(long)]
        limit: Option<usize>,
        /// Those made before too.
        #[arg(long)]
        again: bool,
        /// Only the pipelines earlier runs drew with.
        #[arg(long)]
        used: bool,
        /// (What the processes it starts are given.)
        #[arg(long, hide = true)]
        these: Option<String>,
    },
    /// Prepare, map and bind a title executable; with --execute, run it.
    Run {
        exe: PathBuf,
        /// What unimplemented imports do when called.
        #[arg(long, value_enum, default_value_t = Unimplemented::Trap)]
        unimplemented: Unimplemented,
        /// Disable host pass-through (everything must be implemented by us).
        #[arg(long)]
        strict: bool,
        /// Actually start the title. Without it, stop after binding.
        #[arg(long)]
        execute: bool,
        /// The console model the title sees (device type, memory).
        #[arg(long, value_enum, default_value_t = Console::SeriesX)]
        console: Console,
        /// Arguments for the title, after `--` (it reads the whole command
        /// line; a title that relaunches itself passes some).
        #[arg(last = true)]
        title_args: Vec<String>,
    },
    /// Run a captured frame's GPU work again (diagnostics builds; see
    /// docs/replay.md).
    #[cfg(all(windows, feature = "diagnostics"))]
    Replay(replay::Replay),
    /// Show the resolved configuration and check it.
    Config,
    /// Check NVIDIA Streamline, DLSS, DLSS Frame Generation and AMD FSR 3 on this PC.
    #[cfg(windows)]
    UpscalerCheck {
        /// The output size, `<width>x<height>`.
        #[arg(long, default_value = "2560x1440")]
        output: String,
        /// Make the host device and run DLSS on it.
        #[arg(long)]
        device: bool,
        /// Present for three seconds with frame generation (with --device).
        #[arg(long)]
        present: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Unimplemented {
    Trap,
    Zero,
}

#[derive(Clone, Copy, ValueEnum)]
enum Console {
    SeriesX,
    SeriesS,
}

fn main() -> Result<()> {
    #[cfg(windows)]
    match_d3d12_core();
    #[cfg(windows)]
    victory_hle::reserve_upload_space();
    victory_core::logging::init("info");
    // The audio recorder `run` starts for `VICTORY_AUDIO_CAPTURE`.
    #[cfg(all(windows, feature = "diagnostics"))]
    if victory_hle::modules::audio::capture::child() {
        return Ok(());
    }
    let cli = Cli::parse();
    match cli.command {
        Command::Inspect {
            file,
            imports,
            exports,
        } => inspect::run(&file, imports, exports),
        Command::Census {
            paths,
            strict,
            missing,
        } => census::run(paths, strict, missing),
        Command::LibExports { lib, list } => inspect::lib_exports(&lib, list),
        Command::ShaderInfo {
            paths,
            verbose,
            disasm,
        } => shader::run(paths, verbose || disasm, disasm),
        Command::Translate {
            paths,
            validate,
            pipelines,
            list,
            jobs,
            warm,
        } => {
            // `VICTORY_VIEW_LIMIT=<null slot>`: translate as on a host whose view heap is capped
            if let Some(n) = std::env::var("VICTORY_VIEW_LIMIT").ok().and_then(|v| v.parse().ok()) {
                victory_shader::emit::set_view_heap_limit(n);
            }
            let wave = translate::host_wave::select(
                std::env::var("VICTORY_HOST_WAVE").ok().as_deref(),
                std::env::var("VICTORY_HOST_MAX_WAVE").ok().as_deref(),
                |message| eprintln!("{message}"),
            );
            if let Some(file) = warm {
                return translate::warm(paths, &file, jobs, wave);
            }
            translate::run(paths, validate, pipelines, list, jobs, wave)
        }
        Command::Shaders { paths, jobs, list } => translate::precompile(paths, jobs, list),
        #[cfg(windows)]
        Command::Warm { jobs, limit, again, used, these } => warm::run(jobs, limit, again, used, these),
        Command::Run {
            exe,
            unimplemented,
            strict,
            execute,
            console,
            title_args,
        } => {
            victory_hle::runtime::set_title_args(title_args);
            run::run(&exe, unimplemented, strict, execute, console)
        }
        #[cfg(all(windows, feature = "diagnostics"))]
        Command::Replay(options) => replay::run(options),
        Command::Config => show_config(),
        #[cfg(windows)]
        Command::UpscalerCheck { output, device, present } => {
            let size = output
                .split_once(['x', 'X'])
                .and_then(|(w, h)| Some([w.trim().parse().ok()?, h.trim().parse().ok()?]))
                .ok_or_else(|| anyhow::anyhow!("--output {output}: not <width>x<height>"))?;
            for line in victory_hle::streamline::check(size, device || present, present) {
                println!("{line}");
            }
            Ok(())
        }
    }
}

fn show_config() -> Result<()> {
    let cfg = victory_core::Config::load()?;
    let status = |p: &Option<PathBuf>| match p {
        Some(p) if p.exists() => format!("{}", p.display()),
        Some(p) => format!("{}  (does not exist)", p.display()),
        None => "(not set)".to_owned(),
    };
    println!(
        "config file : {}",
        cfg.source
            .as_ref()
            .map_or("(none)".into(), |p| p.display().to_string())
    );
    println!("game_dir    : {}", status(&cfg.game_dir));
    println!("cache_dir   : {}", cfg.cache_dir.display());
    println!("data_dir    : {}", cfg.data_dir.display());
    for t in &cfg.test_titles {
        println!("test_title  : {}", status(&Some(t.clone())));
    }
    for p in cfg
        .game_dir
        .iter()
        .chain(&cfg.test_titles)
        .chain([&cfg.cache_dir, &cfg.data_dir])
    {
        if let Err(e) = victory_core::ensure_outside_repo(p) {
            println!("POLICY VIOLATION: {e}");
        }
    }
    Ok(())
}

/// Paths given on the command line, or the configured game + test titles.
pub(crate) fn default_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    if !paths.is_empty() {
        return Ok(paths);
    }
    let cfg = victory_core::Config::load()?;
    let all: Vec<PathBuf> = cfg.game_dir.into_iter().chain(cfg.test_titles).collect();
    anyhow::ensure!(
        !all.is_empty(),
        "no paths given and none configured (see victory.example.toml)"
    );
    Ok(all)
}

#[cfg(test)]
mod tests {
    fn core_bytes(major: u32, minor: u32) -> Vec<u8> {
        let mut b = vec![0u8; 40];
        for w in [0xFEEF_04BD_u32, 0x0001_0000, (major << 16) | minor, 2 << 16] {
            b.extend_from_slice(&w.to_le_bytes());
        }
        b.extend_from_slice(&[0u8; 16]);
        b
    }

    #[test]
    fn agility_versions_are_read_from_the_file() {
        assert_eq!(super::agility_version(&core_bytes(1, 620)), Some(620));
        assert_eq!(super::agility_version(&core_bytes(1, 614)), Some(614));
        assert_eq!(super::agility_version(&core_bytes(10, 0)), None);
        assert_eq!(super::agility_version(&[0u8; 64]), None);
        assert_eq!(super::agility_version(&core_bytes(1, 620)[..48]), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_own_runtime_tells_its_version() {
        let system = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap();
        if let Ok(dll) = std::fs::read(system.join("System32").join("D3D12Core.dll")) {
            let v = super::exported_sdk_version(&dll).expect("System32's D3D12Core.dll exports D3D12SDKVersion");
            assert!(v >= 4, "{v}");
            assert!((4..=614).contains(&super::os_runtime()));
        }
        assert_eq!(super::exported_sdk_version(&[0u8; 64]), None);
        assert_eq!(super::exported_sdk_version(b"MZ"), None);
    }
}
