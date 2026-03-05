use captura_capture::Frame;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, thiserror::Error)]
pub enum EncoderError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Encoder init failed: {0}")]
    InitFailed(String),

    #[error("Frame queue is full (max {0} frames)")]
    QueueFull(usize),

    #[error("Encoding error: {0}")]
    EncodingError(String),

    #[error("Encoder already finished")]
    AlreadyFinished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
    Mp4,
    Mov,
    Gif,
}

impl VideoFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            VideoFormat::Mp4 => "mp4",
            VideoFormat::Mov => "mov",
            VideoFormat::Gif => "gif",
        }
    }
}

pub struct EncoderConfig {
    pub output_path: PathBuf,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub width: u32,
    pub height: u32,
    pub format: VideoFormat,
}

pub struct EncodedFile {
    pub path: PathBuf,
    pub duration_secs: f64,
    pub size_bytes: u64,
    pub frame_count: u64,
}

const MAX_QUEUE_SIZE: usize = 120;

enum EncoderMsg {
    Frame(Frame),
    Finish,
}

pub struct Encoder {
    config: EncoderConfig,
    tx: Option<mpsc::SyncSender<EncoderMsg>>,
    thread: Option<std::thread::JoinHandle<Result<EncodedFile, EncoderError>>>,
    aborted: Arc<AtomicBool>,
}

impl Encoder {
    /// Initialize the encoder and open the output file.
    pub fn new(config: EncoderConfig) -> Result<Self, EncoderError> {
        let (tx, rx) = mpsc::sync_channel::<EncoderMsg>(MAX_QUEUE_SIZE);
        let aborted = Arc::new(AtomicBool::new(false));
        let aborted_clone = aborted.clone();

        let output_path = config.output_path.clone();
        let fps = config.fps;
        let format = config.format;
        let width = config.width;
        let height = config.height;

        let thread = std::thread::spawn(move || {
            encode_loop(rx, &output_path, fps, format, width, height, aborted_clone)
        });

        Ok(Self {
            config,
            tx: Some(tx),
            thread: Some(thread),
            aborted,
        })
    }

    /// Push a single frame into the encoder. Non-blocking; frames are queued internally.
    pub fn push_frame(&self, frame: Frame) -> Result<(), EncoderError> {
        let tx = self.tx.as_ref().ok_or(EncoderError::AlreadyFinished)?;
        tx.try_send(EncoderMsg::Frame(frame))
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => EncoderError::QueueFull(MAX_QUEUE_SIZE),
                mpsc::TrySendError::Disconnected(_) => {
                    EncoderError::EncodingError("encoder thread died".to_string())
                }
            })
    }

    /// Finalize the file and flush all pending frames. Consumes self.
    pub fn finish(mut self) -> Result<EncodedFile, EncoderError> {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(EncoderMsg::Finish);
        }
        match self.thread.take() {
            Some(handle) => handle
                .join()
                .map_err(|_| EncoderError::EncodingError("encoder thread panicked".to_string()))?,
            None => Err(EncoderError::AlreadyFinished),
        }
    }

    /// Abort the encode. Deletes the partial output file. Consumes self.
    pub fn abort(mut self) {
        self.aborted.store(true, Ordering::Relaxed);
        drop(self.tx.take());
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        if self.config.output_path.exists() {
            if let Err(e) = std::fs::remove_file(&self.config.output_path) {
                log::warn!("Failed to delete partial file on abort: {e}");
            }
        }
    }
}

fn encode_loop(
    rx: mpsc::Receiver<EncoderMsg>,
    output_path: &PathBuf,
    fps: u32,
    format: VideoFormat,
    width: u32,
    height: u32,
    aborted: Arc<AtomicBool>,
) -> Result<EncodedFile, EncoderError> {
    match format {
        VideoFormat::Gif => encode_gif(rx, output_path, fps, width, height, aborted),
        VideoFormat::Mp4 | VideoFormat::Mov => {
            encode_video_ffmpeg(rx, output_path, fps, format, width, height, aborted)
        }
    }
}

fn encode_gif(
    rx: mpsc::Receiver<EncoderMsg>,
    output_path: &PathBuf,
    fps: u32,
    width: u32,
    height: u32,
    aborted: Arc<AtomicBool>,
) -> Result<EncodedFile, EncoderError> {
    use gif::{Encoder as GifEncoder, Frame as GifFrame, Repeat};
    use std::fs::File;

    let file = File::create(output_path)?;
    let mut encoder = GifEncoder::new(file, width as u16, height as u16, &[])
        .map_err(|e| EncoderError::InitFailed(format!("gif encoder: {e}")))?;
    encoder
        .set_repeat(Repeat::Infinite)
        .map_err(|e| EncoderError::EncodingError(format!("gif repeat: {e}")))?;

    let capped_fps = fps.min(15);
    let delay = (100.0 / capped_fps as f64) as u16; // GIF delay is in centiseconds
    let mut frame_count: u64 = 0;

    for msg in rx {
        if aborted.load(Ordering::Relaxed) {
            break;
        }
        match msg {
            EncoderMsg::Frame(frame) => {
                // Convert BGRA to RGBA
                let mut rgba = Vec::with_capacity(frame.data.len());
                for chunk in frame.data.chunks_exact(4) {
                    rgba.push(chunk[2]); // R
                    rgba.push(chunk[1]); // G
                    rgba.push(chunk[0]); // B
                    rgba.push(chunk[3]); // A
                }

                let mut gif_frame =
                    GifFrame::from_rgba_speed(width as u16, height as u16, &mut rgba, 10);
                gif_frame.delay = delay;

                encoder.write_frame(&gif_frame).map_err(|e| {
                    EncoderError::EncodingError(format!("write gif frame: {e}"))
                })?;
                frame_count += 1;
            }
            EncoderMsg::Finish => break,
        }
    }

    let size_bytes = std::fs::metadata(output_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let duration_secs = frame_count as f64 / capped_fps as f64;

    Ok(EncodedFile {
        path: output_path.clone(),
        duration_secs,
        size_bytes,
        frame_count,
    })
}

fn encode_video_ffmpeg(
    rx: mpsc::Receiver<EncoderMsg>,
    output_path: &PathBuf,
    fps: u32,
    _format: VideoFormat,
    width: u32,
    height: u32,
    aborted: Arc<AtomicBool>,
) -> Result<EncodedFile, EncoderError> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("ffmpeg")
        .args([
            "-y",
            "-f", "rawvideo",
            "-pix_fmt", "bgra",
            "-s", &format!("{width}x{height}"),
            "-r", &fps.to_string(),
            "-i", "pipe:0",
            "-c:v", "libx264",
            "-preset", "ultrafast",
            "-pix_fmt", "yuv420p",
            "-movflags", "+faststart",
        ])
        .arg(output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            EncoderError::InitFailed(format!(
                "ffmpeg not found or failed to start: {e}. Install ffmpeg to enable video recording."
            ))
        })?;

    let mut stdin = child.stdin.take().ok_or_else(|| {
        EncoderError::InitFailed("failed to open ffmpeg stdin".to_string())
    })?;

    let mut frame_count: u64 = 0;

    for msg in rx {
        if aborted.load(Ordering::Relaxed) {
            break;
        }
        match msg {
            EncoderMsg::Frame(frame) => {
                if let Err(e) = stdin.write_all(&frame.data) {
                    log::error!("Failed to write frame to ffmpeg: {e}");
                    break;
                }
                frame_count += 1;
            }
            EncoderMsg::Finish => break,
        }
    }

    drop(stdin);
    let _ = child.wait();

    let size_bytes = std::fs::metadata(output_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let duration_secs = frame_count as f64 / fps as f64;

    Ok(EncodedFile {
        path: output_path.clone(),
        duration_secs,
        size_bytes,
        frame_count,
    })
}
