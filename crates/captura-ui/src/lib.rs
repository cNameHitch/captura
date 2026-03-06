use captura_capture::CaptureRegion;
use captura_config::Config;
use captura_hotkeys::HotkeyManager;
use captura_storage::{CaptureType, StorageManager};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Instant;

const MAX_RECENT_CAPTURES: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("Config error: {0}")]
    Config(#[from] captura_config::ConfigError),

    #[error("Capture error: {0}")]
    Capture(#[from] captura_capture::CaptureError),

    #[error("Storage error: {0}")]
    Storage(#[from] captura_storage::StorageError),

    #[error("UI error: {0}")]
    Ui(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Idle,
    Recording,
}

/// Commands sent from background threads to the main event loop.
#[derive(Debug)]
enum AppCommand {
    TakeScreenshot,
    TakeRegionScreenshot,
    ToggleRecording,
    ToggleRegionRecording,
    UpdateConfig(Config),
}

struct RecordingSession {
    /// The screencapture child process performing the recording.
    child: Child,
    output_path: PathBuf,
    started_at: Instant,
}

pub struct App {
    config: Config,
    storage: StorageManager,
    state: AppState,
    recent_captures: VecDeque<PathBuf>,
    recording_session: Option<RecordingSession>,
}

impl App {
    pub fn new(config: Config) -> Self {
        let storage = StorageManager::new(config.output.clone());
        Self {
            config,
            storage,
            state: AppState::Idle,
            recent_captures: VecDeque::with_capacity(MAX_RECENT_CAPTURES),
            recording_session: None,
        }
    }

    pub fn state(&self) -> AppState {
        self.state
    }

    pub fn recent_captures(&self) -> &VecDeque<PathBuf> {
        &self.recent_captures
    }

    fn add_recent_capture(&mut self, path: PathBuf) {
        if self.recent_captures.len() >= MAX_RECENT_CAPTURES {
            self.recent_captures.pop_back();
        }
        self.recent_captures.push_front(path);
    }

    pub fn update_config(&mut self, config: Config) {
        self.storage.update_config(config.output.clone());
        self.config = config;
    }

    /// Take a screenshot of the full display using macOS screencapture.
    pub fn take_screenshot(&mut self) -> Result<PathBuf, UiError> {
        if self.config.capture.screenshot_delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(
                self.config.capture.screenshot_delay_ms,
            ));
        }

        let path = self.storage.resolve_path(CaptureType::Screenshot)?;

        let region = CaptureRegion::Display(self.config.capture.display_index);
        captura_capture::screencapture_to_file(
            &path,
            &region,
            self.config.capture.include_cursor,
        )?;

        log::info!("Screenshot saved: {}", path.display());
        self.add_recent_capture(path.clone());
        self.post_capture_actions(&path);
        Ok(path)
    }

    /// Take an interactive region screenshot using macOS screencapture -i.
    pub fn take_region_screenshot_interactive(&mut self) -> Result<Option<PathBuf>, UiError> {
        let path = self.storage.resolve_path(CaptureType::RegionScreenshot)?;

        let captured = captura_capture::screencapture_region_interactive(&path)?;
        if !captured {
            log::info!("Region screenshot cancelled by user");
            return Ok(None);
        }

        log::info!("Region screenshot saved: {}", path.display());
        self.add_recent_capture(path.clone());
        self.post_capture_actions(&path);
        Ok(Some(path))
    }

    /// Start recording using macOS native screencapture -v.
    pub fn start_recording(&mut self) -> Result<(), UiError> {
        if self.state == AppState::Recording {
            return Ok(());
        }

        let path = self.storage.resolve_path(CaptureType::Recording)?;

        // Use macOS screencapture -v for video recording.
        // This handles Screen Recording permissions natively.
        let mut cmd = Command::new("screencapture");
        cmd.arg("-v"); // video mode
        cmd.arg("-D").arg(format!("{}", self.config.capture.display_index + 1));
        if self.config.capture.include_cursor {
            cmd.arg("-C"); // capture cursor
        }
        if self.config.recording.capture_microphone {
            cmd.arg("-A"); // capture audio
        }
        cmd.arg(&path);
        // Pipe stdin so screencapture doesn't read keyboard input
        // (it stops on "any character" otherwise)
        cmd.stdin(std::process::Stdio::piped());

        let child = cmd.spawn().map_err(|e| {
            UiError::Ui(format!("Failed to start screencapture: {e}"))
        })?;

        log::info!("Recording started: {}", path.display());

        self.recording_session = Some(RecordingSession {
            child,
            output_path: path,
            started_at: Instant::now(),
        });

        self.state = AppState::Recording;
        Ok(())
    }

    /// Start an interactive region recording.
    /// Uses a helper to let the user select a region, then records with screencapture -v -R.
    pub fn start_region_recording(&mut self) -> Result<(), UiError> {
        if self.state == AppState::Recording {
            return Ok(());
        }

        // Find the select-region helper next to the binary
        let helper = find_helper("select-region");

        let output = Command::new(&helper)
            .output()
            .map_err(|e| UiError::Ui(format!("Failed to run region selector: {e}")))?;

        if !output.status.success() {
            log::info!("Region recording cancelled by user");
            return Err(UiError::Ui("Region selection cancelled".to_string()));
        }

        let rect_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        log::info!("Selected region: {rect_str}");

        let path = self.storage.resolve_path(CaptureType::Recording)?;

        let mut cmd = Command::new("screencapture");
        cmd.arg("-v");
        cmd.arg("-R").arg(&rect_str);
        if self.config.recording.capture_microphone {
            cmd.arg("-g");
        }
        cmd.arg(&path);
        cmd.stdin(std::process::Stdio::piped());

        let child = cmd.spawn().map_err(|e| {
            UiError::Ui(format!("Failed to start screencapture: {e}"))
        })?;

        log::info!("Region recording started: {}", path.display());

        self.recording_session = Some(RecordingSession {
            child,
            output_path: path,
            started_at: Instant::now(),
        });

        self.state = AppState::Recording;
        Ok(())
    }

    /// Stop recording by sending SIGINT to screencapture (graceful stop).
    pub fn stop_recording(&mut self) -> Result<Option<PathBuf>, UiError> {
        if self.state != AppState::Recording {
            return Ok(None);
        }

        let session = self.recording_session.take();
        self.state = AppState::Idle;

        if let Some(mut session) = session {
            let elapsed = session.started_at.elapsed().as_secs_f64();

            // Send SIGINT to screencapture for graceful stop (finalizes the file)
            #[cfg(unix)]
            {
                let pid = session.child.id();
                unsafe {
                    libc::kill(pid as i32, libc::SIGINT);
                }
            }

            // Wait for the process to finish writing
            match session.child.wait() {
                Ok(status) => {
                    log::info!(
                        "Recording saved: {} ({:.1}s, exit: {})",
                        session.output_path.display(),
                        elapsed,
                        status
                    );
                }
                Err(e) => {
                    log::error!("Failed to wait for screencapture: {e}");
                }
            }

            if session.output_path.exists() {
                self.add_recent_capture(session.output_path.clone());
                self.post_capture_actions(&session.output_path);
                return Ok(Some(session.output_path));
            } else {
                log::error!("Recording file was not created");
            }
        }

        Ok(None)
    }

    /// Toggle recording on/off.
    pub fn toggle_recording(&mut self) -> Result<Option<PathBuf>, UiError> {
        match self.state {
            AppState::Idle => {
                self.start_recording()?;
                Ok(None)
            }
            AppState::Recording => self.stop_recording(),
        }
    }

    /// Get elapsed recording time in seconds.
    pub fn recording_elapsed_secs(&self) -> Option<f64> {
        self.recording_session
            .as_ref()
            .map(|s| s.started_at.elapsed().as_secs_f64())
    }

    /// Open the last captured file in Finder.
    pub fn open_last_capture(&self) -> Result<(), UiError> {
        if let Some(path) = self.recent_captures.front() {
            StorageManager::reveal_in_finder(path)?;
        }
        Ok(())
    }

    /// Open the output folder.
    pub fn open_capture_folder(&self) -> Result<(), UiError> {
        StorageManager::reveal_in_finder(&self.config.output.folder)?;
        Ok(())
    }

    fn post_capture_actions(&self, path: &PathBuf) {
        if self.config.ui.reveal_in_finder {
            let _ = StorageManager::reveal_in_finder(path);
        }
        if self.config.ui.copy_path_to_clipboard {
            copy_to_clipboard(path);
        }
        if self.config.ui.show_save_notification {
            show_notification(path);
        }
    }
}

/// Copy the actual file to the clipboard so it can be pasted into apps.
fn copy_to_clipboard(path: &PathBuf) {
    #[cfg(target_os = "macos")]
    {
        let path_str = path.to_string_lossy();
        // Use NSPasteboard via osascript to copy the file itself (not the path)
        let script = format!(
            r#"use framework "AppKit"
set pb to current application's NSPasteboard's generalPasteboard()
pb's clearContents()
set fileURL to current application's NSURL's fileURLWithPath:"{path_str}"
pb's writeObjects:{{fileURL}}"#
        );
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output();
    }
}

fn show_notification(path: &PathBuf) {
    #[cfg(target_os = "macos")]
    {
        let filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();

        let script = format!(
            r#"display notification "Saved: {filename}" with title "Captura" sound name "default""#
        );
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .spawn();
    }
}

/// Build and run the tao event loop with a system tray icon and menu.
/// Uses channels so the hotkey thread sends commands to the main thread,
/// avoiding the need to share non-Send types across threads.
pub fn run_event_loop(config: Config) -> Result<(), UiError> {
    use std::sync::mpsc;
    use tao::event::{Event, StartCause};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::menu::{IconMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::TrayIconBuilder;

    let (cmd_tx, cmd_rx) = mpsc::channel::<AppCommand>();

    let mut app = App::new(config.clone());

    let event_loop = EventLoopBuilder::new().build();

    // Build tray menu
    let menu = Menu::new();

    let screenshot_item = MenuItem::new("Take Screenshot          Ctrl+Shift+1", true, None);
    let region_item = MenuItem::new("Capture Region            Ctrl+Shift+2", true, None);
    let record_item = MenuItem::new("Start Recording          Ctrl+Shift+3", true, None);
    let region_record_item = MenuItem::new("Record Region             Ctrl+Shift+4", true, None);

    // Recent Captures submenu with 5 pre-allocated slots
    let recent_submenu = Submenu::new("Recent Captures", true);
    let recent_empty_item = MenuItem::new("No captures yet", false, None);
    let _ = recent_submenu.append(&recent_empty_item);

    let mut recent_items: Vec<IconMenuItem> = Vec::with_capacity(MAX_RECENT_CAPTURES);
    for _ in 0..MAX_RECENT_CAPTURES {
        let item = IconMenuItem::new("", false, None, None);
        recent_items.push(item);
    }

    let open_folder_item = MenuItem::new("Open Capture Folder", true, None);
    let quit_item = MenuItem::new("Quit Captura", true, None);

    let _ = menu.append(&screenshot_item);
    let _ = menu.append(&region_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&record_item);
    let _ = menu.append(&region_record_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&recent_submenu);
    let _ = menu.append(&open_folder_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit_item);

    let screenshot_id = screenshot_item.id().clone();
    let region_id = region_item.id().clone();
    let record_id = record_item.id().clone();
    let region_record_id = region_record_item.id().clone();
    let recent_item_ids: Vec<_> = recent_items.iter().map(|i| i.id().clone()).collect();
    let open_folder_id = open_folder_item.id().clone();
    let quit_id = quit_item.id().clone();

    let icon = create_tray_icon();

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Captura")
        .with_icon(icon)
        .with_menu_on_left_click(true)
        .build()
        .map_err(|e| UiError::Ui(format!("failed to create tray icon: {e}")))?;

    // Helper closure to sync the Recent Captures submenu with app state
    let update_recent_menu =
        |app: &App, submenu: &Submenu, items: &[IconMenuItem], empty_item: &MenuItem| {
            let captures = app.recent_captures();

            if captures.is_empty() {
                // Show "No captures yet" placeholder
                let _ = empty_item.set_enabled(false);
                // Remove any old items, re-add placeholder
                for item in items.iter() {
                    let _ = submenu.remove(item);
                }
                // Ensure placeholder is in submenu
                let _ = submenu.remove(empty_item);
                let _ = submenu.append(empty_item);
                return;
            }

            // Remove placeholder
            let _ = submenu.remove(empty_item);

            // Update each slot
            for (i, item) in items.iter().enumerate() {
                if let Some(path) = captures.get(i) {
                    let filename = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let _ = item.set_text(&format!("  Copy: {filename}"));
                    let _ = item.set_enabled(true);
                    item.set_icon(generate_thumbnail(path));
                    // Ensure it's in the submenu
                    let _ = submenu.remove(item);
                    let _ = submenu.append(item);
                } else {
                    let _ = submenu.remove(item);
                }
            }
        };

    // Hotkey thread
    let hk_tx = cmd_tx.clone();
    let hk_config = config.clone();
    std::thread::spawn(move || {
        let mut hk_mgr = match HotkeyManager::new() {
            Ok(m) => m,
            Err(e) => {
                log::error!("Failed to init hotkey manager: {e}");
                return;
            }
        };

        let _ = hk_mgr.register("screenshot", &hk_config.hotkeys.screenshot);
        let _ = hk_mgr.register("toggle_recording", &hk_config.hotkeys.toggle_recording);
        let _ = hk_mgr.register("region_screenshot", &hk_config.hotkeys.region_screenshot);
        let _ = hk_mgr.register("region_recording", &hk_config.hotkeys.region_recording);

        loop {
            match hk_mgr.next_event() {
                Ok(id) => {
                    let cmd = match id.as_str() {
                        "screenshot" => AppCommand::TakeScreenshot,
                        "toggle_recording" => AppCommand::ToggleRecording,
                        "region_screenshot" => AppCommand::TakeRegionScreenshot,
                        "region_recording" => AppCommand::ToggleRegionRecording,
                        _ => continue,
                    };
                    if hk_tx.send(cmd).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    log::error!("Hotkey event error: {e}");
                    break;
                }
            }
        }
    });

    // Config watcher
    let watcher_tx = cmd_tx;
    let _watcher = Config::watch(move |new_config| {
        let _ = watcher_tx.send(AppCommand::UpdateConfig(new_config));
    });

    let menu_rx = MenuEvent::receiver().clone();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        let mut menu_changed = false;

        // Process commands from hotkey/config threads
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                AppCommand::TakeScreenshot => {
                    if let Err(e) = app.take_screenshot() {
                        log::error!("Screenshot failed: {e}");
                    } else {
                        menu_changed = true;
                    }
                }
                AppCommand::TakeRegionScreenshot => {
                    match app.take_region_screenshot_interactive() {
                        Ok(Some(_)) => menu_changed = true,
                        Ok(None) => {}
                        Err(e) => log::error!("Region screenshot failed: {e}"),
                    }
                }
                AppCommand::ToggleRecording => {
                    match app.state() {
                        AppState::Idle => {
                            if let Err(e) = app.start_recording() {
                                log::error!("Start recording failed: {e}");
                            } else {
                                let _ = record_item
                                    .set_text("● Stop Recording          Ctrl+Shift+3");
                                let _ = region_record_item.set_enabled(false);
                            }
                        }
                        AppState::Recording => {
                            if let Err(e) = app.stop_recording() {
                                log::error!("Stop recording failed: {e}");
                            } else {
                                menu_changed = true;
                            }
                            let _ = record_item
                                .set_text("Start Recording          Ctrl+Shift+3");
                            let _ = region_record_item.set_enabled(true);
                        }
                    }
                }
                AppCommand::ToggleRegionRecording => {
                    match app.state() {
                        AppState::Idle => {
                            if let Err(e) = app.start_region_recording() {
                                log::error!("Start region recording failed: {e}");
                            } else {
                                let _ = region_record_item
                                    .set_text("● Stop Recording          Ctrl+Shift+4");
                                let _ = record_item.set_enabled(false);
                            }
                        }
                        AppState::Recording => {
                            if let Err(e) = app.stop_recording() {
                                log::error!("Stop recording failed: {e}");
                            } else {
                                menu_changed = true;
                            }
                            let _ = region_record_item
                                .set_text("Record Region             Ctrl+Shift+4");
                            let _ = record_item.set_enabled(true);
                        }
                    }
                }
                AppCommand::UpdateConfig(new_config) => {
                    app.update_config(new_config);
                    log::info!("Config reloaded");
                }
            }
        }

        // Process menu events
        while let Ok(menu_event) = menu_rx.try_recv() {
            if menu_event.id == screenshot_id {
                if let Err(e) = app.take_screenshot() {
                    log::error!("Screenshot failed: {e}");
                } else {
                    menu_changed = true;
                }
            } else if menu_event.id == region_id {
                match app.take_region_screenshot_interactive() {
                    Ok(Some(_)) => menu_changed = true,
                    Ok(None) => {}
                    Err(e) => log::error!("Region screenshot failed: {e}"),
                }
            } else if menu_event.id == record_id {
                match app.state() {
                    AppState::Idle => {
                        if let Err(e) = app.start_recording() {
                            log::error!("Start recording failed: {e}");
                        } else {
                            let _ =
                                record_item.set_text("● Stop Recording          Ctrl+Shift+3");
                            let _ = region_record_item.set_enabled(false);
                        }
                    }
                    AppState::Recording => {
                        if let Err(e) = app.stop_recording() {
                            log::error!("Stop recording failed: {e}");
                        } else {
                            menu_changed = true;
                        }
                        let _ = record_item.set_text("Start Recording          Ctrl+Shift+3");
                        let _ = region_record_item.set_enabled(true);
                    }
                }
            } else if menu_event.id == region_record_id {
                match app.state() {
                    AppState::Idle => {
                        if let Err(e) = app.start_region_recording() {
                            log::error!("Start region recording failed: {e}");
                        } else {
                            let _ = region_record_item
                                .set_text("● Stop Recording          Ctrl+Shift+4");
                            let _ = record_item.set_enabled(false);
                        }
                    }
                    AppState::Recording => {
                        if let Err(e) = app.stop_recording() {
                            log::error!("Stop recording failed: {e}");
                        } else {
                            menu_changed = true;
                        }
                        let _ = region_record_item
                            .set_text("Record Region             Ctrl+Shift+4");
                        let _ = record_item.set_enabled(true);
                    }
                }
            } else if menu_event.id == open_folder_id {
                if let Err(e) = app.open_capture_folder() {
                    log::error!("Open capture folder failed: {e}");
                }
            } else if menu_event.id == quit_id {
                let _ = app.stop_recording();
                *control_flow = ControlFlow::Exit;
            } else {
                // Check if it's a recent capture item — copy path to clipboard
                for (i, rid) in recent_item_ids.iter().enumerate() {
                    if menu_event.id == *rid {
                        if let Some(path) = app.recent_captures().get(i) {
                            copy_to_clipboard(path);
                            log::info!("Copied to clipboard: {}", path.display());
                            show_notification_copied(path);
                        }
                        break;
                    }
                }
            }
        }

        if menu_changed {
            update_recent_menu(&app, &recent_submenu, &recent_items, &recent_empty_item);
        }

        if let Event::NewEvents(StartCause::Init) = event {
            log::info!("Captura UI initialized");
        }
    });
}

fn show_notification_copied(path: &PathBuf) {
    #[cfg(target_os = "macos")]
    {
        let filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();

        let script = format!(
            r#"display notification "Path copied: {filename}" with title "Captura""#
        );
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .spawn();
    }
}

/// Find a helper binary. Looks next to the current executable, then in helpers/.
fn find_helper(name: &str) -> PathBuf {
    // Next to the executable
    if let Ok(exe) = std::env::current_exe() {
        let dir = exe.parent().unwrap_or(std::path::Path::new("."));
        let candidate = dir.join(name);
        if candidate.exists() {
            return candidate;
        }
    }
    // Fallback: helpers/ relative to working directory
    let candidate = PathBuf::from(format!("helpers/{name}"));
    if candidate.exists() {
        return candidate;
    }
    // Last resort: just the name, hope it's in PATH
    PathBuf::from(name)
}

/// Generate a thumbnail icon from a capture file for use in menu items.
/// Returns None if the file can't be loaded or isn't a supported image.
fn generate_thumbnail(path: &std::path::Path) -> Option<tray_icon::menu::Icon> {
    let ext = path.extension()?.to_str()?.to_lowercase();

    let img = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "bmp" | "gif" | "tiff" | "webp" => {
            image::open(path).ok()?.into_rgba8()
        }
        "mp4" | "mov" | "mkv" => {
            // For videos, extract a frame via ffmpeg to a temp file
            let tmp = std::env::temp_dir().join("captura_thumb.png");
            let status = std::process::Command::new("ffmpeg")
                .args(["-y", "-i"])
                .arg(path)
                .args(["-vframes", "1", "-vf", "scale=36:-1"])
                .arg(&tmp)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .ok()?;
            if !status.success() {
                return None;
            }
            let img = image::open(&tmp).ok()?.into_rgba8();
            let _ = std::fs::remove_file(&tmp);
            img
        }
        _ => return None,
    };

    // Resize to fit within 18x18 (menu bar appropriate size)
    let thumb = image::imageops::resize(&img, 18, 18, image::imageops::FilterType::Lanczos3);
    let width = thumb.width();
    let height = thumb.height();
    let rgba = thumb.into_raw();

    tray_icon::menu::Icon::from_rgba(rgba, width, height).ok()
}

/// Create a simple 22x22 camera icon for the menu bar.
fn create_tray_icon() -> tray_icon::Icon {
    let size = 22u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];

    // Draw a simple camera shape (white pixels on transparent background)
    // Body: rounded rectangle from (3,8) to (19,18)
    for y in 8..=18 {
        for x in 3..=19 {
            let idx = ((y * size + x) * 4) as usize;
            rgba[idx] = 255;     // R
            rgba[idx + 1] = 255; // G
            rgba[idx + 2] = 255; // B
            rgba[idx + 3] = 200; // A
        }
    }

    // Viewfinder bump: (8,5) to (14,8)
    for y in 5..=8 {
        for x in 8..=14 {
            let idx = ((y * size + x) * 4) as usize;
            rgba[idx] = 255;
            rgba[idx + 1] = 255;
            rgba[idx + 2] = 255;
            rgba[idx + 3] = 200;
        }
    }

    // Lens: hollow circle center (11,13) radius 3 — cut out inner pixels
    let cx = 11.0f64;
    let cy = 13.0f64;
    for y in 9..=17 {
        for x in 7..=15 {
            let dx = x as f64 - cx;
            let dy = y as f64 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= 2.5 {
                let idx = ((y * size + x as u32) * 4) as usize;
                rgba[idx + 3] = 0; // transparent (cut out lens)
            }
        }
    }

    tray_icon::Icon::from_rgba(rgba, size, size).expect("failed to create tray icon")
}
