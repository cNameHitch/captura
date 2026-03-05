use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("Display not found: index {0}")]
    DisplayNotFound(usize),

    #[error("Capture failed: {0}")]
    CaptureFailed(String),

    #[error("Permission denied: screen recording permission required")]
    PermissionDenied,

    #[error("Stream error: {0}")]
    StreamError(String),
}

/// A raw captured frame.
pub struct Frame {
    /// Raw BGRA pixel data, row-major.
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Monotonic capture timestamp in nanoseconds.
    pub timestamp_ns: u64,
}

/// Describes the region to capture.
#[derive(Debug, Clone)]
pub enum CaptureRegion {
    /// Capture an entire display by index.
    Display(usize),
    /// Capture a specific pixel rectangle on a display.
    Rect {
        display: usize,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
}

pub struct Capturer {
    region: CaptureRegion,
    include_cursor: bool,
}

/// Dropping this handle stops the capture stream.
pub struct CaptureStreamHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for CaptureStreamHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Capturer {
    pub fn new(region: CaptureRegion, include_cursor: bool) -> Result<Self, CaptureError> {
        // Validate display index
        let display_index = match &region {
            CaptureRegion::Display(idx) => *idx,
            CaptureRegion::Rect { display, .. } => *display,
        };

        let display_count = Self::display_count()?;
        if display_index >= display_count {
            return Err(CaptureError::DisplayNotFound(display_index));
        }

        Ok(Self {
            region,
            include_cursor,
        })
    }

    /// Capture a single frame immediately.
    pub fn capture_frame(&self) -> Result<Frame, CaptureError> {
        self.platform_capture_frame()
    }

    /// Begin a continuous capture stream at the target FPS.
    pub fn start_stream(
        &self,
        fps: u32,
        on_frame: impl Fn(Frame) + Send + 'static,
    ) -> Result<CaptureStreamHandle, CaptureError> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let region = self.region.clone();
        let include_cursor = self.include_cursor;

        let thread = std::thread::spawn(move || {
            let frame_interval = std::time::Duration::from_secs_f64(1.0 / fps as f64);
            let capturer = match Capturer::new(region, include_cursor) {
                Ok(c) => c,
                Err(e) => {
                    log::error!("Failed to create capturer for stream: {e}");
                    return;
                }
            };

            while !stop_clone.load(Ordering::Relaxed) {
                let start = Instant::now();
                match capturer.platform_capture_frame() {
                    Ok(frame) => on_frame(frame),
                    Err(e) => {
                        log::warn!("Frame capture failed, dropping frame: {e}");
                    }
                }
                let elapsed = start.elapsed();
                if elapsed < frame_interval {
                    std::thread::sleep(frame_interval - elapsed);
                }
            }
        });

        Ok(CaptureStreamHandle {
            stop,
            thread: Some(thread),
        })
    }

    fn display_count() -> Result<usize, CaptureError> {
        #[cfg(target_os = "macos")]
        {
            Self::macos_display_count()
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Fallback: assume at least one display
            Ok(1)
        }
    }

    #[cfg(target_os = "macos")]
    fn macos_display_count() -> Result<usize, CaptureError> {
        use core_graphics::display::CGDisplay;
        let displays = CGDisplay::active_displays()
            .map_err(|e| CaptureError::CaptureFailed(format!("enumerate displays: {e}")))?;
        Ok(displays.len())
    }

    fn platform_capture_frame(&self) -> Result<Frame, CaptureError> {
        #[cfg(target_os = "macos")]
        {
            self.macos_capture_frame()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(CaptureError::CaptureFailed(
                "Screen capture not implemented for this platform".to_string(),
            ))
        }
    }

    #[cfg(target_os = "macos")]
    fn macos_capture_frame(&self) -> Result<Frame, CaptureError> {
        use core_graphics::display::{CGDisplay, CGRect, CGPoint, CGSize};

        let display_id = self.get_display_id()?;
        let display = CGDisplay::new(display_id);

        let image = match &self.region {
            CaptureRegion::Display(_) => CGDisplay::screenshot(
                display.bounds(),
                core_graphics::window::kCGWindowListOptionOnScreenOnly,
                core_graphics::window::kCGNullWindowID,
                core_graphics::display::kCGWindowImageDefault,
            ),
            CaptureRegion::Rect {
                x, y, width, height, ..
            } => {
                // Clamp to display bounds
                let bounds = display.bounds();
                let max_w = bounds.size.width as u32;
                let max_h = bounds.size.height as u32;
                let clamped_w = (*width).min(max_w.saturating_sub(*x));
                let clamped_h = (*height).min(max_h.saturating_sub(*y));

                let rect = CGRect::new(
                    &CGPoint::new(*x as f64, *y as f64),
                    &CGSize::new(clamped_w as f64, clamped_h as f64),
                );
                CGDisplay::screenshot(
                    rect,
                    core_graphics::window::kCGWindowListOptionOnScreenOnly,
                    core_graphics::window::kCGNullWindowID,
                    core_graphics::display::kCGWindowImageDefault,
                )
            }
        };

        let image = image.ok_or_else(|| {
            CaptureError::CaptureFailed("CGDisplay::screenshot returned null".to_string())
        })?;

        let width = image.width() as u32;
        let height = image.height() as u32;
        let raw_data = image.data();
        let bytes_per_row = image.bytes_per_row();
        let raw_len = raw_data.len() as usize;

        // Convert to packed BGRA
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for row in 0..height {
            let row_start = (row as usize) * bytes_per_row;
            let row_end = row_start + (width as usize * 4);
            if row_end <= raw_len {
                data.extend_from_slice(&raw_data[row_start..row_end]);
            }
        }

        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        Ok(Frame {
            data,
            width,
            height,
            timestamp_ns,
        })
    }

    #[cfg(target_os = "macos")]
    fn get_display_id(&self) -> Result<u32, CaptureError> {
        use core_graphics::display::CGDisplay;

        let idx = match &self.region {
            CaptureRegion::Display(i) => *i,
            CaptureRegion::Rect { display, .. } => *display,
        };

        let displays = CGDisplay::active_displays()
            .map_err(|e| CaptureError::CaptureFailed(format!("enumerate displays: {e}")))?;

        displays
            .get(idx)
            .copied()
            .ok_or(CaptureError::DisplayNotFound(idx))
    }
}
