use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("TOML serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),

    #[error("File watcher error: {0}")]
    Watch(#[from] notify::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub output: OutputConfig,
    pub capture: CaptureConfig,
    pub recording: RecordingConfig,
    pub hotkeys: HotkeyConfig,
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output: OutputConfig::default(),
            capture: CaptureConfig::default(),
            recording: RecordingConfig::default(),
            hotkeys: HotkeyConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub folder: PathBuf,
    pub filename_template: String,
    pub screenshot_format: String,
    pub video_format: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        let folder = dirs::picture_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Pictures"))
            .join("Captura");
        Self {
            folder,
            filename_template: "{type}_{date}_{time}".to_string(),
            screenshot_format: "png".to_string(),
            video_format: "mp4".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    pub include_cursor: bool,
    pub screenshot_delay_ms: u64,
    pub display_index: usize,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            include_cursor: true,
            screenshot_delay_ms: 0,
            display_index: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingConfig {
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub capture_microphone: bool,
    pub capture_system_audio: bool,
    pub max_duration_secs: u64,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            fps: 30,
            bitrate_kbps: 0,
            capture_microphone: false,
            capture_system_audio: false,
            max_duration_secs: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub screenshot: String,
    pub toggle_recording: String,
    pub region_screenshot: String,
    pub region_recording: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            screenshot: "Ctrl+Shift+1".to_string(),
            toggle_recording: "Ctrl+Shift+3".to_string(),
            region_screenshot: "Ctrl+Shift+2".to_string(),
            region_recording: "Ctrl+Shift+4".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub show_save_notification: bool,
    pub reveal_in_finder: bool,
    pub copy_path_to_clipboard: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            show_save_notification: true,
            reveal_in_finder: false,
            copy_path_to_clipboard: false,
        }
    }
}

impl Config {
    /// Return the path to the config file.
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
            .join("captura")
            .join("config.toml")
    }

    /// Load config from disk. Creates default config + directory if missing.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::config_path();

        if !path.exists() {
            let config = Config::default();
            config.save()?;
            return Ok(config);
        }

        let contents = std::fs::read_to_string(&path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Save the current config to disk, atomically (write to temp, rename).
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let contents = toml::to_string_pretty(self)?;
        let tmp_path = path.with_extension("toml.tmp");
        std::fs::write(&tmp_path, &contents)?;
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    }

    /// Watch the config file for changes. Calls `callback` on any change.
    /// Spawns a background thread. Returns a handle that stops watching on drop.
    pub fn watch<F: Fn(Config) + Send + 'static>(
        callback: F,
    ) -> Result<ConfigWatcher, ConfigError> {
        let path = Self::config_path();
        let watch_dir = path
            .parent()
            .ok_or_else(|| {
                ConfigError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "config directory not found",
                ))
            })?
            .to_path_buf();

        let (tx, rx) = mpsc::channel::<Event>();

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = tx.send(event);
                }
            },
            notify::Config::default(),
        )?;

        watcher.watch(&watch_dir, RecursiveMode::NonRecursive)?;

        let config_path = path.clone();
        let handle = std::thread::spawn(move || {
            let debounce = Duration::from_millis(200);
            let mut last_fire = Instant::now() - debounce;

            for event in rx {
                let dominated_by_config = event.paths.iter().any(|p| p == &config_path);
                let is_modify = matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_)
                );

                if dominated_by_config && is_modify {
                    let now = Instant::now();
                    if now.duration_since(last_fire) >= debounce {
                        last_fire = now;
                        if let Ok(config) = Config::load() {
                            callback(config);
                        }
                    }
                }
            }
        });

        Ok(ConfigWatcher {
            _watcher: watcher,
            _thread: Some(handle),
        })
    }
}

pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
    _thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_roundtrip() {
        let config = Config::default();
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.recording.fps, 30);
        assert_eq!(deserialized.output.screenshot_format, "png");
        assert_eq!(deserialized.hotkeys.screenshot, "Ctrl+Shift+1");
        assert!(deserialized.ui.show_save_notification);
    }

    #[test]
    fn test_missing_keys_use_defaults() {
        let partial = r#"
[capture]
include_cursor = false
"#;
        let config: Config = toml::from_str(partial).unwrap();
        assert!(!config.capture.include_cursor);
        assert_eq!(config.recording.fps, 30);
        assert_eq!(config.hotkeys.screenshot, "Ctrl+Shift+1");
    }

    #[test]
    fn test_unknown_keys_ignored() {
        let with_unknown = r#"
[capture]
include_cursor = true
some_future_key = "hello"

[unknown_section]
foo = "bar"
"#;
        // Should not error — unknown keys silently ignored
        let config: Config = toml::from_str(with_unknown).unwrap();
        assert!(config.capture.include_cursor);
    }
}
