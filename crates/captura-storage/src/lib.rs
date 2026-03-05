use captura_config::OutputConfig;
use chrono::Local;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid filename template: {0}")]
    InvalidTemplate(String),

    #[error("Failed to open in file manager: {0}")]
    RevealFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureType {
    Screenshot,
    Recording,
    RegionScreenshot,
}

impl CaptureType {
    fn type_token(&self) -> &'static str {
        match self {
            CaptureType::Screenshot => "screenshot",
            CaptureType::Recording => "recording",
            CaptureType::RegionScreenshot => "region",
        }
    }

    fn extension<'a>(&self, config: &'a OutputConfig) -> &'a str {
        match self {
            CaptureType::Screenshot | CaptureType::RegionScreenshot => {
                &config.screenshot_format
            }
            CaptureType::Recording => &config.video_format,
        }
    }
}

static SESSION_INDEX: AtomicU32 = AtomicU32::new(1);

pub struct StorageManager {
    config: OutputConfig,
}

impl StorageManager {
    pub fn new(config: OutputConfig) -> Self {
        Self { config }
    }

    /// Update the config (e.g. after a config reload).
    pub fn update_config(&mut self, config: OutputConfig) {
        self.config = config;
    }

    /// Ensure the output folder exists, creating it recursively if needed.
    pub fn ensure_output_dir(&self) -> Result<(), StorageError> {
        std::fs::create_dir_all(&self.config.folder)?;
        Ok(())
    }

    /// Resolve a full output path for the given capture type.
    pub fn resolve_path(&self, capture_type: CaptureType) -> Result<PathBuf, StorageError> {
        self.ensure_output_dir()?;

        let now = Local::now();
        let template = &self.config.filename_template;

        let filename = template
            .replace("{type}", capture_type.type_token())
            .replace("{date}", &now.format("%Y-%m-%d").to_string())
            .replace("{time}", &now.format("%H-%M-%S").to_string())
            .replace("{timestamp}", &now.timestamp().to_string())
            .replace(
                "{index}",
                &format!("{:03}", SESSION_INDEX.fetch_add(1, Ordering::Relaxed)),
            );

        if filename.is_empty() {
            return Err(StorageError::InvalidTemplate(
                "template produced empty filename".to_string(),
            ));
        }

        // Check for invalid characters
        if filename.contains('/') || filename.contains('\0') {
            return Err(StorageError::InvalidTemplate(format!(
                "template produced invalid filename: {filename}"
            )));
        }

        let ext = capture_type.extension(&self.config);
        let base = self.config.folder.join(&filename);

        // Try the base path first
        let mut candidate = base.with_extension(ext);
        if !candidate.exists() {
            return Ok(candidate);
        }

        // Append _N suffix to avoid collisions
        let mut n = 2u32;
        loop {
            let suffixed = format!("{filename}_{n}");
            candidate = self.config.folder.join(&suffixed).with_extension(ext);
            if !candidate.exists() {
                return Ok(candidate);
            }
            n += 1;
        }
    }

    /// Open the containing folder of a given path in Finder (macOS) or file manager (Linux).
    pub fn reveal_in_finder(path: &Path) -> Result<(), StorageError> {
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .arg("-R")
                .arg(path)
                .spawn()
                .map_err(|e| StorageError::RevealFailed(e.to_string()))?;
        }

        #[cfg(target_os = "linux")]
        {
            let dir = path.parent().unwrap_or(path);
            std::process::Command::new("xdg-open")
                .arg(dir)
                .spawn()
                .map_err(|e| StorageError::RevealFailed(e.to_string()))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config(dir: &Path) -> OutputConfig {
        OutputConfig {
            folder: dir.to_path_buf(),
            filename_template: "{type}_{date}_{time}".to_string(),
            screenshot_format: "png".to_string(),
            video_format: "mp4".to_string(),
        }
    }

    #[test]
    fn test_resolve_path_screenshot() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = StorageManager::new(test_config(tmp.path()));
        let path = mgr.resolve_path(CaptureType::Screenshot).unwrap();
        assert!(path.to_str().unwrap().contains("screenshot_"));
        assert_eq!(path.extension().unwrap(), "png");
    }

    #[test]
    fn test_resolve_path_recording() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = StorageManager::new(test_config(tmp.path()));
        let path = mgr.resolve_path(CaptureType::Recording).unwrap();
        assert!(path.to_str().unwrap().contains("recording_"));
        assert_eq!(path.extension().unwrap(), "mp4");
    }

    #[test]
    fn test_resolve_path_region() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = StorageManager::new(test_config(tmp.path()));
        let path = mgr.resolve_path(CaptureType::RegionScreenshot).unwrap();
        assert!(path.to_str().unwrap().contains("region_"));
        assert_eq!(path.extension().unwrap(), "png");
    }

    #[test]
    fn test_collision_avoidance() {
        let tmp = tempfile::tempdir().unwrap();
        let config = OutputConfig {
            folder: tmp.path().to_path_buf(),
            filename_template: "fixed_name".to_string(),
            screenshot_format: "png".to_string(),
            video_format: "mp4".to_string(),
        };
        let mgr = StorageManager::new(config);

        let p1 = mgr.resolve_path(CaptureType::Screenshot).unwrap();
        std::fs::write(&p1, b"").unwrap();

        let p2 = mgr.resolve_path(CaptureType::Screenshot).unwrap();
        assert_ne!(p1, p2);
        assert!(p2.to_str().unwrap().contains("_2"));
    }

    #[test]
    fn test_all_tokens_expanded() {
        let tmp = tempfile::tempdir().unwrap();
        let config = OutputConfig {
            folder: tmp.path().to_path_buf(),
            filename_template: "{type}_{date}_{time}_{timestamp}_{index}".to_string(),
            screenshot_format: "png".to_string(),
            video_format: "mp4".to_string(),
        };
        let mgr = StorageManager::new(config);
        let path = mgr.resolve_path(CaptureType::Screenshot).unwrap();
        let name = path.file_stem().unwrap().to_str().unwrap();

        assert!(!name.contains("{type}"));
        assert!(!name.contains("{date}"));
        assert!(!name.contains("{time}"));
        assert!(!name.contains("{timestamp}"));
        assert!(!name.contains("{index}"));
    }
}
