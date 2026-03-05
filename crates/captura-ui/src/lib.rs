use captura_audio::{AudioCapturer, AudioConfig, AudioHandle};
use captura_capture::{CaptureRegion, CaptureStreamHandle, Capturer, Frame};
use captura_config::Config;
use captura_encoder::{EncodedFile, Encoder, EncoderConfig, VideoFormat};
use captura_hotkeys::HotkeyManager;
use captura_storage::{CaptureType, StorageManager};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("Config error: {0}")]
    Config(#[from] captura_config::ConfigError),

    #[error("Capture error: {0}")]
    Capture(#[from] captura_capture::CaptureError),

    #[error("Encoder error: {0}")]
    Encoder(#[from] captura_encoder::EncoderError),

    #[error("Storage error: {0}")]
    Storage(#[from] captura_storage::StorageError),

    #[error("Audio error: {0}")]
    Audio(#[from] captura_audio::AudioError),

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
    _stream_handle: CaptureStreamHandle,
    _audio_handle: Option<AudioHandle>,
    encoder: Option<Encoder>,
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

    /// Take a screenshot of the full display.
    pub fn take_screenshot(&mut self) -> Result<PathBuf, UiError> {
        if self.config.capture.screenshot_delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(
                self.config.capture.screenshot_delay_ms,
            ));
        }

        let capturer = Capturer::new(
            CaptureRegion::Display(self.config.capture.display_index),
            self.config.capture.include_cursor,
        )?;

        let frame = capturer.capture_frame()?;
        let path = self.storage.resolve_path(CaptureType::Screenshot)?;

        save_frame_as_image(&frame, &path, &self.config.output.screenshot_format)?;

        self.last_capture = Some(path.clone());
        self.post_capture_actions(&path);
        Ok(path)
    }

    /// Take a screenshot of a specific region.
    pub fn take_region_screenshot(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<PathBuf, UiError> {
        let capturer = Capturer::new(
            CaptureRegion::Rect {
                display: self.config.capture.display_index,
                x,
                y,
                width,
                height,
            },
            self.config.capture.include_cursor,
        )?;

        let frame = capturer.capture_frame()?;
        let path = self.storage.resolve_path(CaptureType::RegionScreenshot)?;

        save_frame_as_image(&frame, &path, &self.config.output.screenshot_format)?;

        self.last_capture = Some(path.clone());
        self.post_capture_actions(&path);
        Ok(path)
    }

    /// Start recording.
    pub fn start_recording(&mut self) -> Result<(), UiError> {
        if self.state == AppState::Recording {
            return Ok(());
        }

        let path = self.storage.resolve_path(CaptureType::Recording)?;
        let capturer = Capturer::new(
            CaptureRegion::Display(self.config.capture.display_index),
            self.config.capture.include_cursor,
        )?;

        // Capture one frame to get dimensions
        let probe = capturer.capture_frame()?;
        let width = probe.width;
        let height = probe.height;

        let format = match self.config.output.video_format.as_str() {
            "mov" => VideoFormat::Mov,
            "gif" => VideoFormat::Gif,
            _ => VideoFormat::Mp4,
        };

        let encoder = Encoder::new(EncoderConfig {
            output_path: path.clone(),
            fps: self.config.recording.fps,
            bitrate_kbps: self.config.recording.bitrate_kbps,
            width,
            height,
            format,
        })?;

        // Share encoder with the stream callback via Arc<Mutex>
        let encoder_shared = Arc::new(Mutex::new(Some(encoder)));
        let encoder_for_stream = encoder_shared.clone();

        let stream = capturer.start_stream(self.config.recording.fps, move |frame| {
            if let Ok(guard) = encoder_for_stream.lock() {
                if let Some(enc) = guard.as_ref() {
                    if let Err(e) = enc.push_frame(frame) {
                        log::warn!("Failed to push frame: {e}");
                    }
                }
            }
        })?;

        // Audio capture
        let audio_handle = if self.config.recording.capture_microphone
            || self.config.recording.capture_system_audio
        {
            let audio = AudioCapturer::new(AudioConfig {
                capture_microphone: self.config.recording.capture_microphone,
                capture_system_audio: self.config.recording.capture_system_audio,
                ..AudioConfig::default()
            })?;
            Some(audio.start(|_samples| {
                // Audio muxing into the video is future work.
            })?)
        } else {
            None
        };

        // Take the encoder out of the Arc for storage in the session.
        // The stream callback's Arc clone still holds a reference, but we've already
        // started the stream. We'll take it back when stopping.
        let encoder_owned = Arc::try_unwrap(encoder_shared)
            .ok()
            .and_then(|m| m.into_inner().ok())
            .flatten();

        self.recording_session = Some(RecordingSession {
            _stream_handle: stream,
            _audio_handle: audio_handle,
            encoder: encoder_owned,
            started_at: Instant::now(),
        });

        self.state = AppState::Recording;
        self.last_capture = Some(path);
        Ok(())
    }

    /// Stop recording and finalize the file.
    pub fn stop_recording(&mut self) -> Result<Option<EncodedFile>, UiError> {
        if self.state != AppState::Recording {
            return Ok(None);
        }

        let session = self.recording_session.take();
        self.state = AppState::Idle;

        if let Some(mut session) = session {
            // Drop stream + audio handles first to stop capture
            drop(session._stream_handle);
            drop(session._audio_handle);

            if let Some(encoder) = session.encoder.take() {
                let encoded = encoder.finish()?;
                self.last_capture = Some(encoded.path.clone());
                self.post_capture_actions(&encoded.path);
                return Ok(Some(encoded));
            }
        }

        Ok(None)
    }

    /// Toggle recording on/off.
    pub fn toggle_recording(&mut self) -> Result<Option<EncodedFile>, UiError> {
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

fn save_frame_as_image(frame: &Frame, path: &PathBuf, format: &str) -> Result<(), UiError> {
    // Frame data is BGRA, convert to RGBA for image crate
    let mut rgba = Vec::with_capacity(frame.data.len());
    for chunk in frame.data.chunks_exact(4) {
        rgba.push(chunk[2]); // R
        rgba.push(chunk[1]); // G
        rgba.push(chunk[0]); // B
        rgba.push(chunk[3]); // A
    }

    let img = image::RgbaImage::from_raw(frame.width, frame.height, rgba)
        .ok_or_else(|| UiError::Ui("failed to create image from frame data".to_string()))?;

    let img = image::DynamicImage::ImageRgba8(img);

    match format {
        "jpg" | "jpeg" => img.save_with_format(path, image::ImageFormat::Jpeg),
        "webp" => img.save_with_format(path, image::ImageFormat::WebP),
        _ => img.save_with_format(path, image::ImageFormat::Png),
    }
    .map_err(|e| UiError::Ui(format!("save image: {e}")))?;

    Ok(())
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

/// Build and run the tao event loop with muda menus.
/// Uses channels so the hotkey thread sends commands to the main thread,
/// avoiding the need to share non-Send types across threads.
pub fn run_event_loop(config: Config) -> Result<(), UiError> {
    use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use std::sync::mpsc;
    use tao::event::{Event, StartCause};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};

    let (cmd_tx, cmd_rx) = mpsc::channel::<AppCommand>();

    // App lives entirely on the main thread — no Arc<Mutex> needed
    let mut app = App::new(config.clone());

    let event_loop = EventLoopBuilder::new().build();

    // Build menu
    let menu = Menu::new();

    let screenshot_item = MenuItem::new("Take Screenshot\tCmd+Shift+3", true, None);
    let region_item = MenuItem::new("Capture Region\tCmd+Shift+4", true, None);
    let record_item = MenuItem::new("Start Recording\tCmd+Shift+5", true, None);
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
                    // Region selection UI is future work; full-screen fallback
                    if let Err(e) = app.take_screenshot() {
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
        while let Ok(event) = menu_rx.try_recv() {
            if event.id == screenshot_id {
                if let Err(e) = app.take_screenshot() {
                    log::error!("Screenshot failed: {e}");
                }
            } else if event.id == region_id {
                if let Err(e) = app.take_screenshot() {
                    log::error!("Region screenshot failed: {e}");
                }
            } else if event.id == record_id {
                if let Err(e) = app.toggle_recording() {
                    log::error!("Toggle recording failed: {e}");
                }
            } else if event.id == open_last_id {
                if let Err(e) = app.open_last_capture() {
                    log::error!("Open last capture failed: {e}");
                }
            } else if event.id == open_folder_id {
                if let Err(e) = app.open_capture_folder() {
                    log::error!("Open capture folder failed: {e}");
                }
            } else if event.id == quit_id {
                let _ = app.stop_recording();
                *control_flow = ControlFlow::Exit;
            }
        }

        if let Event::NewEvents(StartCause::Init) = event {
            log::info!("Captura UI initialized");
        }
    });
}
