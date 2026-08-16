// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

#![forbid(non_ascii_idents)]
#![deny(unsafe_code)]

use asterctl::cfg::{MonitorConfig, Panel, load_custom_panel};
use asterctl::render::PanelRenderer;
use asterctl::sensors::{read_filter_file, read_key_value_file, start_file_slurper};
use asterctl::shm::SharedMemoryProvider;
use asterctl::{cfg, img};
use asterctl_lcd::{AooScreen, AooScreenBuilder, DISPLAY_SIZE};

use anyhow::anyhow;
use clap::Parser;
use env_logger::Env;
use log::{debug, error, info};
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// AOOSTAR WTR MAX and GEM12+ PRO screen control.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Serial device, for example, "/dev/cu.usbserial-AB0KOHLS". Takes priority over --usb option.
    #[arg(short, long)]
    device: Option<String>,

    /// USB serial UART "vid:pid" in hex notation (lsusb output). Default: 416:90A1
    #[arg(short, long)]
    usb: Option<String>,

    /// Switch display on and exit. This will show the last displayed image.
    #[arg(long)]
    on: bool,

    /// Switch display off and exit.
    #[arg(long)]
    off: bool,

    /// Image to display, other sizes than 960x376 will be scaled.
    #[arg(short, long)]
    image: Option<String>,

    /// AOOSTAR-X json configuration file to parse.
    ///
    /// The configuration file will be loaded from the `config_dir` directory if no full path is
    /// specified.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Include one or more additional custom panels into the base configuration.
    ///
    /// Specify the path to the panel directory containing panel.json and fonts / img subdirectories.
    #[arg(short, long)]
    panels: Option<Vec<PathBuf>>,

    /// Configuration directory containing configuration files and background images
    /// specified in the `config` file.
    #[arg(long, default_value_t = String::from("cfg"))]
    config_dir: String, // default_value_t requires Display trait which PathBuf does not implement

    /// Font directory for fonts specified in the `config` file.
    #[arg(long, default_value_t = String::from("fonts"))]
    font_dir: String,

    /// Single sensor value input file or directory for multiple sensor input files.
    #[arg(long, default_value_t = String::from("cfg/sensors"))]
    sensor_path: String,

    /// Also read hardware sensor values from the "AOOSTAR_HW_STATS" shared
    /// memory region (written by `HwBridge.exe --shm`). Sensor files are
    /// still read as before; shared-memory values win on key conflicts.
    #[arg(long)]
    shm: bool,

    /// Theme index 0-3 (Default, Cyberpunk, Interstellar, Cartoon).
    /// Overrides the `theme` value in the config file's `setup` section.
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=3))]
    theme: Option<u32>,

    /// Sensor identifier mapping file. Ignored if the file does not exist.
    ///
    /// The configuration file will be loaded from the `config_dir` directory if no full path is
    /// specified.
    #[arg(long, default_value_t = String::from("sensor-mapping.cfg"))]
    sensor_mapping: String,

    /// Switch off display n seconds after loading image or running demo.
    #[arg(short, long)]
    off_after: Option<u32>,

    /// Path to a display-state control file written by aster-launcher
    /// (plain text `on` or `off`). While set, the panel loop follows the
    /// file: `off` turns the display off and skips rendering until the file
    /// reads `on` again. The serial port stays open either way, so the
    /// display can be woken without a restart. The file also doubles as the
    /// launcher's heartbeat: if it is not rewritten for ~10s (the launcher
    /// closed or was killed), the display is switched off and this process
    /// exits to free the serial port.
    #[arg(long)]
    display_state: Option<PathBuf>,

    /// Path to a "panel unresponsive" marker file written by aster-launcher
    /// (`cfg/uart.stuck`). On a failed display init this process writes the
    /// file (best effort) so the launcher can escalate — re-enumerate the
    /// USB UART and restart us — instead of letting the init retries burn
    /// out against a panel that needs a USB-level reset to recover. The
    /// file is removed again once the display initializes.
    #[arg(long)]
    stuck_file: Option<PathBuf>,

    /// Test mode: only write to the display without checking response.
    #[arg(short, long)]
    write_only: bool,

    /// Test mode: save changed images in ./out folder.
    #[arg(short, long)]
    save: bool,

    /// Simulate serial port for testing and development, `--device` and `--usb` options are ignored.
    #[arg(long)]
    simulate: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    // initialize display with given UART port parameter
    let mut builder = AooScreenBuilder::new();
    builder.no_init_check(args.write_only);
    let mut screen = if args.simulate {
        builder.simulate()?
    } else if let Some(device) = args.device {
        builder.open_device(&device)?
    } else if let Some(usb) = args.usb {
        builder.open_usb_id(&usb)?
    } else {
        builder.open_default()?
    };

    // process simple commands
    if args.off {
        screen.off()?;
        return Ok(());
    } else if args.on {
        screen.on()?;
        return Ok(());
    }

    // switch on screen for remaining commands
    if let Err(e) = init_display_with_retry(&mut screen) {
        // The panel is not accepting commands (e.g. its USB endpoint went
        // stale after Modern Standby). Tell the launcher so it can escalate
        // — re-enumerate the USB UART and restart us — instead of letting
        // the init retry budget burn out against a panel that needs a
        // USB-level reset to come back.
        if let Some(path) = args.stuck_file.as_deref() {
            report_stuck(path);
        }
        return Err(e);
    }
    // Display is up: clear any stale marker so a previous failed attempt
    // cannot trigger a needless launcher escalation on the next wake.
    if let Some(path) = args.stuck_file.as_deref() {
        clear_stuck(path);
    }

    if let Some(config) = args.config {
        info!("Starting sensor panel mode");
        let img_save_path = if args.save {
            let img_save_path = PathBuf::from("out");
            fs::create_dir_all(&img_save_path)?;
            Some(img_save_path)
        } else {
            None
        };

        let cfg_dir = PathBuf::from(args.config_dir);
        let font_dir = PathBuf::from(args.font_dir);
        let sensor_path = PathBuf::from(args.sensor_path);
        let mapping_cfg = PathBuf::from(args.sensor_mapping);
        let cfg = load_configuration(&config, &cfg_dir, args.panels, &mapping_cfg, args.theme)?;
        run_sensor_panel(
            &mut screen,
            cfg,
            cfg_dir,
            font_dir,
            sensor_path,
            args.shm,
            args.display_state,
            args.stuck_file,
            img_save_path,
        )?;
        return Ok(());
    }

    if let Some(image) = args.image {
        info!("Loading and displaying background image {image}...");
        let rgb_img = img::load_image(&image, Some(DISPLAY_SIZE))?.to_rgb8();
        let timestamp = Instant::now();
        screen.send_image(&rgb_img)?;
        debug!("Image sent in {}ms", timestamp.elapsed().as_millis());
    }

    if let Some(off) = args.off_after {
        info!("Switching off display in {off}s");
        sleep(Duration::from_secs(off as u64));
        screen.off()?;
    }

    info!("Bye bye!");

    Ok(())
}

/// Display-init retry budget. 1s+2s+4s+8s+16s of backoff plus the init
/// timeouts themselves cover the observed post-resume window in which the
/// LCD takes a while to accept writes after USB re-enumeration (~10s in the
/// common case); when the budget is exhausted the error is returned and the
/// launcher watcher restarts us (with its own backoff for crash-exit loops).
const INIT_RETRY_ATTEMPTS: u32 = 6;

/// Backoff in seconds before the n-th display-init retry: 1, 2, 4, ..., 32.
/// Mirrors the reconnect backoff used by `asterctl-lcd`'s
/// `reconnect_with_retry`. Extracted so the sequence is unit-testable.
fn init_backoff_secs(attempt: u32) -> u64 {
    1u64 << attempt.min(5)
}

/// Writes the "panel unresponsive" marker (`cfg/uart.stuck`) the launcher
/// polls on wake to decide when to re-enumerate the USB UART. Best effort:
/// a marker that cannot be written must not crash the process — the
/// launcher just never escalates.
fn report_stuck(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, "stuck\n");
}

/// [`report_stuck`] for an optional path (the `--stuck-file` flag is not
/// always set). No-op when unset.
fn report_stuck_if_set(path: Option<&Path>) {
    if let Some(path) = path {
        report_stuck(path);
    }
}

/// Removes the stuck marker once the display is initialized, so a stale
/// file from a previous failed attempt cannot trigger a needless launcher
/// escalation on the next wake. Best effort, like [`report_stuck`].
fn clear_stuck(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

/// [`clear_stuck`] for an optional path. No-op when unset.
fn clear_stuck_if_set(path: Option<&Path>) {
    if let Some(path) = path {
        clear_stuck(path);
    }
}

/// Tries `screen.init()`, backing off between attempts ([`init_backoff_secs`])
/// so a display that is still waking up after USB re-enumeration gets a
/// chance to respond instead of the process exiting on the first timeout.
/// Returns the last error once [`INIT_RETRY_ATTEMPTS`] are exhausted.
fn init_display_with_retry(screen: &mut AooScreen) -> anyhow::Result<()> {
    let mut attempt = 0u32;
    loop {
        match screen.init() {
            Ok(()) => return Ok(()),
            Err(e) if attempt + 1 < INIT_RETRY_ATTEMPTS => {
                let delay = Duration::from_secs(init_backoff_secs(attempt));
                error!(
                    "Display init failed: {e:?}; retrying in {}s (attempt {})",
                    delay.as_secs(),
                    attempt + 1
                );
                sleep(delay);
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

fn load_configuration<P: AsRef<Path>, Q: AsRef<Path>, R: AsRef<Path>>(
    config: P,
    config_dir: Q,
    panels: Option<Vec<PathBuf>>,
    sensor_mapping: R,
    theme: Option<u32>,
) -> anyhow::Result<MonitorConfig> {
    let config = config.as_ref();
    let config_dir = config_dir.as_ref();

    let mut cfg = if config.is_absolute() {
        cfg::load_cfg(config)?
    } else {
        cfg::load_cfg(config_dir.join(config))?
    };

    // Theme selection (official AOOSTAR-X parity): the CLI flag wins over
    // the JSON's `setup.theme`; when neither is present, the config's own
    // `mianban` is left untouched (backward compatible for custom configs).
    if let Some(theme) = theme.map(|t| t as i32).or(cfg.setup.theme) {
        cfg.apply_theme(theme);
    }

    if let Some(panels) = panels {
        for panel in panels {
            cfg.include_custom_panel(load_custom_panel(panel)?);
        }
    }

    let sensor_mapping = sensor_mapping.as_ref();
    let mapping_cfg = if sensor_mapping.is_absolute() {
        sensor_mapping.to_path_buf()
    } else {
        config_dir.join(sensor_mapping)
    };
    if mapping_cfg.is_file() {
        let mut mapping = HashMap::new();
        read_key_value_file(&mapping_cfg, &mut mapping, None)?;
        cfg.set_sensor_mapping(mapping);
    } else {
        info!("Sensor mapping file {mapping_cfg:?} not found");
    }

    cfg.sensor_filter = load_sensor_filter(&mapping_cfg)?;

    Ok(cfg)
}

fn load_sensor_filter(mapping_cfg: &Path) -> anyhow::Result<Option<Vec<Regex>>> {
    if let Some(parent) = mapping_cfg.parent()
        && let Some(file_stem) = mapping_cfg.file_stem()
        && let Some(extension) = mapping_cfg.extension()
    {
        let filter_file = parent
            .join(format!("{}-filter", file_stem.to_string_lossy()))
            .with_extension(extension);

        if filter_file.is_file() {
            info!("Loading sensor filter file {filter_file:?}");
            return read_filter_file(filter_file);
        } else {
            info!("No sensor filter file {filter_file:?} available");
        }
    }

    Ok(None)
}

fn run_sensor_panel<B: Into<PathBuf>>(
    screen: &mut AooScreen,
    mut cfg: MonitorConfig,
    config_dir: B,
    font_dir: B,
    sensor_path: B,
    use_shm: bool,
    display_state: Option<PathBuf>,
    stuck_file: Option<PathBuf>,
    img_save_path: Option<B>,
) -> anyhow::Result<()> {
    let font_dir = font_dir.into();
    let config_dir = config_dir.into();
    let img_save_path = img_save_path.map(|p| p.into());

    // True while the display is actually showing content. The display was
    // just initialized, so it starts on; the state file (if any) is polled
    // below and can turn it off at any time.
    let mut display_on = true;

    let mut renderer = PanelRenderer::new(DISPLAY_SIZE, &font_dir, &config_dir);
    if let Some(img_save_path) = &img_save_path {
        renderer.set_img_save_path(img_save_path);
        renderer.set_save_render_img(true);
        // renderer.set_save_processed_pic(true);
        // renderer.set_save_progress_layer(true);
    }

    let sensor_values: Arc<RwLock<HashMap<String, String>>> = Arc::new(RwLock::new(HashMap::new()));

    start_file_slurper(
        sensor_path,
        sensor_values.clone(),
        cfg.sensor_filter.clone(),
    )?;

    // Shared-memory sensor source (HwBridge): polled on every render
    // iteration; values are merged into the same map as the file slurper.
    let mut shm_provider = if use_shm {
        info!("Shared memory sensor source enabled (AOOSTAR_HW_STATS)");
        Some(SharedMemoryProvider::new())
    } else {
        None
    };

    let refresh = Duration::from_millis((cfg.setup.refresh * 1000f32) as u64);

    let switch_time = cfg
        .setup
        .switch_time
        .as_deref()
        .and_then(|v| f32::from_str(v).ok())
        .map(|v| Duration::from_millis((v * 1000.0) as u64))
        .unwrap_or(Duration::from_secs(5));

    // panel switching loop
    loop {
        let panel = cfg
            .get_next_active_panel()
            .ok_or(anyhow!("No active panel"))?;

        info!("Switching panel: {}", panel.friendly_name());
        let panel_switch_time = Instant::now();

        // active panel refresh loop
        let mut refresh_count = 1;
        loop {
            let upd_start_time = Instant::now();

            // Display-state control (aster-launcher): poll `cfg/display.state`
            // every render tick, and every second while waiting for the next
            // tick. `off` switches the display off and skips rendering until
            // the file reads `on` again; the serial port stays open so the
            // wake-up needs no restart. Missing/unreadable file keeps the
            // display on (backward compatible with launchers that never write
            // one). If the file goes stale the launcher is gone: display off
            // and exit (see `poll_display_state`).
            let mut skip_render = false;
            if let Some(state_file) = display_state.as_deref() {
                if !poll_display_state(screen, state_file, stuck_file.as_deref(), &mut display_on) {
                    info!("aster-launcher no longer running; display switched off, exiting");
                    return Ok(());
                }
                skip_render = !display_on;
            }

            if !skip_render {
                if img_save_path.is_some() {
                    renderer.set_img_suffix(format!("-{refresh_count:02}"));
                }

                // Poll shared memory first so the render below sees the freshest
                // values. The write lock is released before rendering.
                if let Some(provider) = shm_provider.as_mut() {
                    let mut values = sensor_values.write().expect("RwLock is poisoned");
                    provider.update(&mut *values);
                }
                let values = sensor_values.read().expect("RwLock is poisoned");
                if let Err(e) = update_panel(screen, &mut renderer, panel, &values) {
                    // Serial communication failed (e.g. after resume). Don't
                    // exit — drop the stale handle, reopen + re-init the port
                    // with backoff, and continue the panel loop. The frame cache
                    // was cleared by `reconnect`, so the next send is a full
                    // frame that repairs whatever the display was left showing.
                    // Report the failure so the launcher can escalate (USB
                    // remove+rescan) instead of letting the backoff loop spin
                    // against a panel that needs a USB-level reset.
                    error!("Display communication failed: {e:?} — reconnecting with backoff");
                    report_stuck_if_set(stuck_file.as_deref());
                    screen.reconnect_with_retry();
                    clear_stuck_if_set(stuck_file.as_deref());
                }
                drop(values);
            }

            let elapsed = upd_start_time.elapsed();
            if refresh > elapsed {
                let mut remaining = refresh - elapsed;
                if let Some(state_file) = display_state.as_deref() {
                    // Re-poll every second while waiting so launcher death
                    // and manual on/off apply quickly even at long refresh
                    // intervals (e.g. quitting the launcher blanks the
                    // display within ~1s, not after the full refresh).
                    while !remaining.is_zero() {
                        let step = remaining.min(STATE_POLL_STEP);
                        sleep(step);
                        remaining -= step;
                        if !poll_display_state(
                            screen,
                            state_file,
                            stuck_file.as_deref(),
                            &mut display_on,
                        ) {
                            info!(
                                "aster-launcher no longer running; display switched off, exiting"
                            );
                            return Ok(());
                        }
                    }
                } else {
                    sleep(remaining);
                }
            }

            if panel_switch_time.elapsed() >= switch_time {
                break;
            }

            refresh_count += 1;
        }
    }
}

/// How long the display-state file may go without being rewritten before
/// asterctl considers its writer (aster-launcher) dead. The launcher
/// rewrites the file roughly every 2s while it runs; keep this at a
/// generous multiple so a busy system can never cause a false positive.
const LAUNCHER_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);

/// Poll cadence while waiting for the next render tick: the display-state
/// file is re-read every second so launcher death and manual on/off apply
/// quickly even when the panel refresh interval is long (up to 30s).
const STATE_POLL_STEP: Duration = Duration::from_secs(1);

/// Reads the display-state file written by aster-launcher. Returns `true`
/// when the display should be on. A missing or unreadable file means "on":
/// launchers without this feature never write the file, and a transient
/// read error must not blank the display.
fn read_display_state(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) => display_state_text_is_on(&text),
        Err(_) => true,
    }
}

/// Parses the content of a display-state file: exactly `off` (after
/// trimming) turns the display off; anything else keeps it on.
fn display_state_text_is_on(text: &str) -> bool {
    text.trim() != "off"
}

/// True when the display-state file's writer (aster-launcher) is gone:
/// the launcher rewrites the file roughly every 2s while it runs, so a
/// file that has not been touched for [`LAUNCHER_HEARTBEAT_TIMEOUT`] means
/// the launcher closed or was killed. A missing file counts as alive
/// (backward compatible with launchers that never write one).
fn launcher_is_dead(state_file: &Path) -> bool {
    launcher_is_dead_with_timeout(state_file, LAUNCHER_HEARTBEAT_TIMEOUT)
}

/// [`launcher_is_dead`] with an explicit timeout (kept testable).
fn launcher_is_dead_with_timeout(state_file: &Path, timeout: Duration) -> bool {
    match std::fs::metadata(state_file) {
        Ok(meta) => meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > timeout),
        // unreadable metadata or clock trouble → assume the launcher lives
        Err(_) => false,
    }
}

/// Polls the display-state file once and syncs the display to it.
///
/// Returns `true` while aster-launcher is alive and the process should
/// keep running; returns `false` when the launcher's heartbeat is lost,
/// meaning the display has been switched off and the process should exit
/// (releasing the serial port for a relaunched launcher).
///
/// The state file doubles as the launcher heartbeat (see
/// [`launcher_is_dead`]): with the launcher gone, the display is switched
/// off no matter what the file content says.
fn poll_display_state(
    screen: &mut AooScreen,
    state_file: &Path,
    stuck_file: Option<&Path>,
    display_on: &mut bool,
) -> bool {
    if launcher_is_dead(state_file) {
        if *display_on {
            match screen.off() {
                Ok(()) => {
                    info!("Display switched off: aster-launcher heartbeat lost");
                    *display_on = false;
                }
                Err(e) => {
                    error!(
                        "Failed to switch display off after launcher heartbeat loss: {e:?} — reconnecting with backoff"
                    );
                    report_stuck_if_set(stuck_file);
                    screen.reconnect_with_retry();
                    clear_stuck_if_set(stuck_file);
                }
            }
        }
        return false;
    }

    let want_on = read_display_state(state_file);
    if want_on != *display_on {
        let (action, result) = if want_on {
            ("on", screen.on())
        } else {
            ("off", screen.off())
        };
        match result {
            Ok(()) => {
                info!("Display switched {action} by state file");
                *display_on = want_on;
            }
            Err(e) => {
                // Same recovery as a failed frame send below: reopen +
                // re-init the port with backoff so the next poll can apply
                // the state file again. Report the failure so the launcher
                // can escalate (USB remove+rescan).
                error!("Failed to switch display {action}: {e:?} — reconnecting with backoff");
                report_stuck_if_set(stuck_file);
                screen.reconnect_with_retry();
                clear_stuck_if_set(stuck_file);
            }
        }
    }
    true
}

fn update_panel(
    screen: &mut AooScreen,
    renderer: &mut PanelRenderer,
    panel: &Panel,
    values: &HashMap<String, String>,
) -> anyhow::Result<()> {
    debug!("Displaying panel '{}'...", panel.friendly_name());

    match renderer.render(panel, values) {
        Ok(image) => screen.send_image(&image)?,
        Err(e) => error!("Error rendering panel '{}': {e:?}", panel.friendly_name()),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_config(dir: &std::path::Path, theme: Option<i32>, mianban: &[u32]) {
        let panels: Vec<String> = (1..=8)
            .map(|i| {
                format!(
                    r#"{{"img": "default_{i}_index.jpg", "sensor": [{{"mode": 1, "label": "cpu", "value": "", "unit": "", "integerDigits": -1, "decimalDigits": -1, "pic": "", "x": 10, "y": 10}}]}}"#
                )
            })
            .collect();
        let theme_json = theme
            .map(|t| format!(r#", "theme": {t}"#))
            .unwrap_or_default();
        let mianban_json: Vec<String> = mianban.iter().map(|p| p.to_string()).collect();
        let json = format!(
            r#"{{"setup": {{"refresh": 1, "controlParams": true, "controlDiskTemp": true{theme_json}}}, "mianban": [{}], "diy": [{}]}}"#,
            mianban_json.join(","),
            panels.join(",")
        );
        fs::write(dir.join("Monitor3.json"), json).unwrap();
    }

    #[test]
    fn cli_theme_overrides_json_theme() {
        let dir = tempdir().unwrap();
        write_config(dir.path(), Some(2), &[1]);
        let cfg = load_configuration(
            "Monitor3.json",
            dir.path(),
            None,
            dir.path().join("no-such-mapping.cfg"),
            Some(3),
        )
        .unwrap();
        assert_eq!(cfg.active_panels, vec![7, 8]);
    }

    #[test]
    fn json_theme_applies_when_no_cli_flag() {
        let dir = tempdir().unwrap();
        write_config(dir.path(), Some(1), &[1]);
        let cfg = load_configuration(
            "Monitor3.json",
            dir.path(),
            None,
            dir.path().join("no-such-mapping.cfg"),
            None,
        )
        .unwrap();
        assert_eq!(cfg.active_panels, vec![3, 4]);
    }

    #[test]
    fn mianban_untouched_without_theme() {
        let dir = tempdir().unwrap();
        write_config(dir.path(), None, &[1]);
        let cfg = load_configuration(
            "Monitor3.json",
            dir.path(),
            None,
            dir.path().join("no-such-mapping.cfg"),
            None,
        )
        .unwrap();
        assert_eq!(cfg.active_panels, vec![1]);
    }

    #[test]
    fn init_backoff_grows_and_caps() {
        assert_eq!(init_backoff_secs(0), 1);
        assert_eq!(init_backoff_secs(1), 2);
        assert_eq!(init_backoff_secs(2), 4);
        assert_eq!(init_backoff_secs(3), 8);
        assert_eq!(init_backoff_secs(4), 16);
        assert_eq!(init_backoff_secs(5), 32);
        // cap at 32s for any further attempt
        assert_eq!(init_backoff_secs(6), 32);
        assert_eq!(init_backoff_secs(100), 32);
    }

    #[test]
    fn stuck_marker_roundtrips_through_report_and_clear() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("uart.stuck");

        // Reporting creates the parent directory and writes the marker.
        report_stuck(&path);
        assert_eq!(fs::read_to_string(&path).unwrap(), "stuck\n");

        // Clearing removes it; clearing a missing file is a silent no-op.
        clear_stuck(&path);
        assert!(!path.exists());
        clear_stuck(&path);
    }

    #[test]
    fn display_state_text_off_turns_display_off() {
        assert!(!display_state_text_is_on("off\n"));
        assert!(!display_state_text_is_on("off"));
        assert!(!display_state_text_is_on("  off  \n"));
    }

    #[test]
    fn display_state_text_on_or_garbage_keeps_display_on() {
        assert!(display_state_text_is_on("on\n"));
        assert!(display_state_text_is_on("on"));
        assert!(display_state_text_is_on(""));
        assert!(display_state_text_is_on("garbage"));
        // case-sensitive: anything but exactly "off" means on
        assert!(display_state_text_is_on("OFF"));
    }

    #[test]
    fn read_display_state_missing_file_keeps_display_on() {
        let dir = tempdir().unwrap();
        assert!(read_display_state(&dir.path().join("no-such-file")));
    }

    #[test]
    fn read_display_state_reads_file_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("display.state");
        fs::write(&path, "off\n").unwrap();
        assert!(!read_display_state(&path));
        fs::write(&path, "on\n").unwrap();
        assert!(read_display_state(&path));
    }

    #[test]
    fn launcher_heartbeat_missing_file_counts_as_alive() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("no-such-file");
        assert!(!launcher_is_dead(&path));
    }

    #[test]
    fn launcher_heartbeat_fresh_means_alive() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("display.state");
        fs::write(&path, "on\n").unwrap();
        assert!(!launcher_is_dead_with_timeout(
            &path,
            Duration::from_secs(10)
        ));
    }

    #[test]
    fn launcher_heartbeat_stale_means_dead() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("display.state");
        fs::write(&path, "on\n").unwrap();
        // wait past the (tiny) timeout so the file is definitely stale
        std::thread::sleep(Duration::from_millis(20));
        assert!(launcher_is_dead_with_timeout(
            &path,
            Duration::from_millis(5)
        ));
    }
}
