use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("Permission denied: microphone access required")]
    PermissionDenied,

    #[error("Nothing to capture: both microphone and system audio are disabled")]
    NothingToCaptureError,

    #[error("Audio device error: {0}")]
    DeviceError(String),

    #[error("Audio stream error: {0}")]
    StreamError(String),
}

pub struct AudioConfig {
    pub capture_microphone: bool,
    pub capture_system_audio: bool,
    pub sample_rate: u32,
    pub channels: u8,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            capture_microphone: false,
            capture_system_audio: false,
            sample_rate: 44100,
            channels: 2,
        }
    }
}

pub struct AudioCapturer {
    config: AudioConfig,
}

/// Dropping this handle stops audio capture.
pub struct AudioHandle {
    stop: Arc<AtomicBool>,
    _stream: Option<cpal::Stream>,
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

impl AudioCapturer {
    pub fn new(config: AudioConfig) -> Result<Self, AudioError> {
        if !config.capture_microphone && !config.capture_system_audio {
            return Err(AudioError::NothingToCaptureError);
        }
        Ok(Self { config })
    }

    /// Start capturing. Calls `on_samples` with raw f32 interleaved PCM.
    pub fn start(
        &self,
        on_samples: impl Fn(&[f32]) + Send + 'static,
    ) -> Result<AudioHandle, AudioError> {
        let stop = Arc::new(AtomicBool::new(false));

        if self.config.capture_microphone {
            let host = cpal::default_host();
            let device = host
                .default_input_device()
                .ok_or_else(|| AudioError::DeviceError("no input device found".to_string()))?;

            let supported_config = device
                .default_input_config()
                .map_err(|e| AudioError::DeviceError(format!("input config: {e}")))?;

            let stop_clone = stop.clone();
            let stream = device
                .build_input_stream(
                    &supported_config.into(),
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        if !stop_clone.load(Ordering::Relaxed) {
                            on_samples(data);
                        }
                    },
                    |err| {
                        log::error!("Audio stream error: {err}");
                    },
                    None,
                )
                .map_err(|e| AudioError::StreamError(format!("build stream: {e}")))?;

            stream
                .play()
                .map_err(|e| AudioError::StreamError(format!("play stream: {e}")))?;

            return Ok(AudioHandle {
                stop,
                _stream: Some(stream),
            });
        }

        // System audio capture is platform-specific and requires ScreenCaptureKit on macOS 13+.
        // For now, return a no-op handle if only system audio is requested.
        if self.config.capture_system_audio {
            log::warn!("System audio capture is not yet implemented; returning silent handle");
        }

        Ok(AudioHandle {
            stop,
            _stream: None,
        })
    }
}
