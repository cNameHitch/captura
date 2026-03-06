# Captura

A lightweight, native macOS menu bar application for screenshots and screen recording, written in Rust.

Captura lives entirely in the menu bar — no dock icon, no main window. Take a screenshot or start a recording with a global hotkey or two clicks.

## Features

- **Screenshots** — Full display capture, saved as PNG
- **Region Screenshots** — Interactive region selection via macOS screencapture
- **Screen Recording** — Full display video recording to MP4 via macOS screencapture
- **Region Recording** — Select a screen region, then record just that area
- **Global Hotkeys** — Configurable shortcuts that work system-wide
  - `Ctrl+Shift+1` — Take screenshot
  - `Ctrl+Shift+2` — Capture region screenshot
  - `Ctrl+Shift+3` — Start/stop recording
  - `Ctrl+Shift+4` — Start/stop region recording
- **Recent Captures** — Last 5 captures shown in menu with thumbnail previews; click to copy file to clipboard
- **Clipboard Integration** — Copies the actual file (not the path) to the clipboard for easy pasting
- **Auto-save** — Configurable output folder and filename templates
- **Collision-free naming** — Automatic `_N` suffix if a file already exists
- **Config file watching** — Edit `~/Library/Application Support/captura/config.toml` and changes apply instantly
- **Notifications** — macOS notifications on save
- **Single instance** — Lock file prevents multiple instances from running

## Requirements

- macOS 13+ (Ventura or later)
- Rust 1.70+
- [ffmpeg](https://ffmpeg.org/) (optional, used for video thumbnail generation in recent captures menu)

## Building

```sh
cargo build --release

# Compile the region selection helper
swiftc helpers/select-region.swift -o target/release/select-region -framework Cocoa
```

The binary is at `target/release/captura`. The `select-region` helper must be in the same directory for region recording to work.

## Usage

```sh
./target/release/captura
```

On first launch, Captura creates a default config at `~/Library/Application Support/captura/config.toml` and saves captures to `~/Pictures/Captura/`.

### Menu Bar

Click the camera icon in the menu bar to access:

- **Take Screenshot** — Captures the full display
- **Capture Region** — Interactive crosshair selection for a region screenshot
- **Start Recording** — Records the full display (click again or use hotkey to stop)
- **Record Region** — Select a region, then record it (use hotkey or menu to stop)
- **Recent Captures** — Last 5 captures with thumbnail previews; click to copy the file to clipboard
- **Open Capture Folder** — Opens the output folder in Finder
- **Quit Captura** — Stops any active recording and exits

## Configuration

All settings are in `~/Library/Application Support/captura/config.toml`:

```toml
[output]
folder = "/Users/you/Desktop/captura"
filename_template = "{type}_{date}_{time}"
screenshot_format = "png"
video_format = "mp4"

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
screenshot = "Ctrl+Shift+1"
toggle_recording = "Ctrl+Shift+3"
region_screenshot = "Ctrl+Shift+2"
region_recording = "Ctrl+Shift+4"

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

### Hotkey Modifiers

Available modifiers: `Cmd`, `Ctrl`, `Alt` (Option), `Shift`

Keys: `A`-`Z`, `0`-`9`, `F1`-`F20`, `Space`, `Return`

Example: `"Ctrl+Alt+S"`, `"Cmd+Shift+R"`

## Architecture

Rust workspace with 8 crates:

```
crates/
├── captura-app/        # Binary entry point, signal handling, single-instance lock
├── captura-capture/    # Screen capture via Core Graphics + screencapture CLI
├── captura-encoder/    # Video encoding (placeholder, recording uses screencapture)
├── captura-audio/      # Audio capture (placeholder)
├── captura-storage/    # Output path resolution, template expansion
├── captura-config/     # TOML config with load/save/watch
├── captura-hotkeys/    # Global hotkey registration via global-hotkey
└── captura-ui/         # Menu bar UI (tao + tray-icon/muda), app state machine

helpers/
├── select-region.swift # Native macOS region selection overlay (compiled to binary)
└── record.swift        # ScreenCaptureKit recorder (standalone, for reference)
```

## Permissions

On macOS, you must grant **Screen Recording** permission to your terminal app (or the captura binary) in **System Settings > Privacy & Security > Screen Recording**.

| Permission | When Needed |
|---|---|
| Screen Recording | Screenshots and recording |
| Microphone | When `capture_microphone = true` in config |

## License

MIT
