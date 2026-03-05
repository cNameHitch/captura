# Captura

A lightweight, native macOS menu bar application for screenshots and screen recording, written in Rust.

Captura lives entirely in the menu bar — no dock icon, no main window. Take a screenshot or start a recording in two clicks or fewer.

## Features

- **Screenshots** — Full display or region capture, saved as PNG/JPG/WebP
- **Screen Recording** — Continuous capture encoded to MP4/MOV/GIF via ffmpeg
- **Global Hotkeys** — Configurable shortcuts that work system-wide
  - `Cmd+Shift+3` — Take screenshot
  - `Cmd+Shift+4` — Capture region
  - `Cmd+Shift+5` — Start/stop recording
- **Auto-save** — Configurable output folder and filename templates
- **Collision-free naming** — Automatic `_N` suffix if a file already exists
- **Config file watching** — Edit `~/.config/captura/config.toml` and changes apply instantly
- **Notifications** — macOS notifications on save with optional Finder reveal
- **Low overhead** — Idle resource usage under 1% CPU / 30MB RAM

## Requirements

- macOS (primary target)
- Rust 1.70+
- [ffmpeg](https://ffmpeg.org/) (required for MP4/MOV recording)

## Building

```sh
cargo build --release
```

The binary is at `target/release/captura`.

## Usage

```sh
# Run the app (appears in the menu bar)
./target/release/captura
```

On first launch, Captura creates a default config at `~/.config/captura/config.toml` and saves captures to `~/Pictures/Captura/`.

## Configuration

All settings are in `~/.config/captura/config.toml`:

```toml
[output]
folder = "/Users/you/Pictures/Captura"
filename_template = "{type}_{date}_{time}"
screenshot_format = "png"       # png, jpg, webp
video_format = "mp4"            # mp4, mov, gif

[capture]
include_cursor = true
screenshot_delay_ms = 0
display_index = 0

[recording]
fps = 30
bitrate_kbps = 0                # 0 = auto
capture_microphone = false
capture_system_audio = false
max_duration_secs = 0           # 0 = unlimited

[hotkeys]
screenshot = "Cmd+Shift+3"
toggle_recording = "Cmd+Shift+5"
region_screenshot = "Cmd+Shift+4"

[ui]
show_save_notification = true
reveal_in_finder = false
copy_path_to_clipboard = false
```

### Filename Template Tokens

| Token | Expands To | Example |
|---|---|---|
| `{date}` | `YYYY-MM-DD` | `2025-11-01` |
| `{time}` | `HH-MM-SS` | `14-32-07` |
| `{timestamp}` | Unix timestamp | `1730468927` |
| `{type}` | `screenshot`, `recording`, `region` | `screenshot` |
| `{index}` | Auto-incrementing integer | `001` |

## Architecture

Rust workspace with 8 crates:

```
crates/
├── captura-app/        # Binary entry point, signal handling, single-instance lock
├── captura-capture/    # Screen capture via Core Graphics
├── captura-encoder/    # Video encoding (ffmpeg for MP4/MOV, gif crate for GIF)
├── captura-audio/      # Microphone capture via cpal
├── captura-storage/    # Output path resolution, template expansion
├── captura-config/     # TOML config with load/save/watch
├── captura-hotkeys/    # Global hotkey registration via global-hotkey
└── captura-ui/         # Menu bar UI (tao + muda), app state machine
```

## Permissions

Captura requests permissions lazily:

| Permission | When Requested |
|---|---|
| Screen Recording | First screenshot or recording |
| Microphone | When mic capture is enabled in config |

## License

MIT
