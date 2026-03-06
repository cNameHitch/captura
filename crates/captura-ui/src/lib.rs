use captura_capture::CaptureRegion;
use captura_config::Config;
use captura_hotkeys::HotkeyManager;
use captura_storage::{CaptureType, StorageManager};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Instant;

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
    last_capture: Option<PathBuf>,
    recording_session: Option<RecordingSession>,
}

impl App {
    pub fn new(config: Config) -> Self {
        let storage = StorageManager::new(config.output.clone());
        Self {
            config,
            storage,
            state: AppState::Idle,
            last_capture: None,
            recording_session: None,
        }
    }

    pub fn state(&self) -> AppState {
        self.state
    }

    pub fn last_capture(&self) -> Option<&PathBuf> {
        self.last_capture.as_ref()
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
        self.last_capture = Some(path.clone());
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
        self.last_capture = Some(path.clone());
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
                self.last_capture = Some(session.output_path.clone());
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
        if let Some(path) = &self.last_capture {
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

fn copy_to_clipboard(path: &PathBuf) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(path.to_string_lossy().as_bytes());
                }
                child.wait()
            });
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
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::TrayIconBuilder;

    let (cmd_tx, cmd_rx) = mpsc::channel::<AppCommand>();

    // App lives entirely on the main thread — no Arc<Mutex> needed
    let mut app = App::new(config.clone());

    let event_loop = EventLoopBuilder::new().build();

    // Build tray menu
    let menu = Menu::new();

    let screenshot_item = MenuItem::new("Take Screenshot          Cmd+Shift+3", true, None);
    let region_item = MenuItem::new("Capture Region            Cmd+Shift+4", true, None);
    let record_item = MenuItem::new("Start Recording          Cmd+Shift+5", true, None);
    let open_last_item = MenuItem::new("Open Last Capture", false, None);
    let open_folder_item = MenuItem::new("Open Capture Folder", true, None);
    let quit_item = MenuItem::new("Quit Captura", true, None);

    let _ = menu.append(&screenshot_item);
    let _ = menu.append(&region_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&record_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&open_last_item);
    let _ = menu.append(&open_folder_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit_item);

    let screenshot_id = screenshot_item.id().clone();
    let region_id = region_item.id().clone();
    let record_id = record_item.id().clone();
    let open_last_id = open_last_item.id().clone();
    let open_folder_id = open_folder_item.id().clone();
    let quit_id = quit_item.id().clone();

    // Create a simple camera icon (16x16 white on transparent)
    let icon = create_tray_icon();

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Captura")
        .with_icon(icon)
        .with_menu_on_left_click(true)
        .build()
        .map_err(|e| UiError::Ui(format!("failed to create tray icon: {e}")))?;

    // Hotkey thread — sends commands via channel
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

        loop {
            match hk_mgr.next_event() {
                Ok(id) => {
                    let cmd = match id.as_str() {
                        "screenshot" => AppCommand::TakeScreenshot,
                        "toggle_recording" => AppCommand::ToggleRecording,
                        "region_screenshot" => AppCommand::TakeRegionScreenshot,
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

    // Config watcher — sends UpdateConfig commands via channel
    let watcher_tx = cmd_tx;
    let _watcher = Config::watch(move |new_config| {
        let _ = watcher_tx.send(AppCommand::UpdateConfig(new_config));
    });

    let menu_rx = MenuEvent::receiver().clone();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        // Process commands from hotkey/config threads
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                AppCommand::TakeScreenshot => {
                    if let Err(e) = app.take_screenshot() {
                        log::error!("Screenshot failed: {e}");
                    }
                }
                AppCommand::TakeRegionScreenshot => {
                    if let Err(e) = app.take_region_screenshot_interactive() {
                        log::error!("Region screenshot failed: {e}");
                    }
                }
                AppCommand::ToggleRecording => {
                    if let Err(e) = app.toggle_recording() {
                        log::error!("Toggle recording failed: {e}");
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
                }
            } else if menu_event.id == region_id {
                if let Err(e) = app.take_region_screenshot_interactive() {
                    log::error!("Region screenshot failed: {e}");
                }
            } else if menu_event.id == record_id {
                match app.state() {
                    AppState::Idle => {
                        if let Err(e) = app.start_recording() {
                            log::error!("Start recording failed: {e}");
                        } else {
                            let _ = record_item.set_text("● Stop Recording          Cmd+Shift+5");
                        }
                    }
                    AppState::Recording => {
                        if let Err(e) = app.stop_recording() {
                            log::error!("Stop recording failed: {e}");
                        }
                        let _ = record_item.set_text("Start Recording          Cmd+Shift+5");
                    }
                }
            } else if menu_event.id == open_last_id {
                if let Err(e) = app.open_last_capture() {
                    log::error!("Open last capture failed: {e}");
                }
            } else if menu_event.id == open_folder_id {
                if let Err(e) = app.open_capture_folder() {
                    log::error!("Open capture folder failed: {e}");
                }
            } else if menu_event.id == quit_id {
                let _ = app.stop_recording();
                *control_flow = ControlFlow::Exit;
            }

            // Enable "Open Last Capture" once we have one
            if app.last_capture().is_some() {
                let _ = open_last_item.set_enabled(true);
            }
        }

        if let Event::NewEvents(StartCause::Init) = event {
            log::info!("Captura UI initialized");
        }
    });
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
