use captura_config::Config;
use std::path::PathBuf;
use std::process;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Single-instance lock
    let lock_path = lock_file_path();
    if let Err(e) = acquire_lock(&lock_path) {
        eprintln!("Captura is already running: {e}");
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("osascript")
                .arg("-e")
                .arg(r#"display notification "Captura is already running" with title "Captura""#)
                .spawn();
        }
        process::exit(1);
    }

    // Load config
    let config = match Config::load() {
        Ok(c) => c,
        Err(captura_config::ConfigError::Parse(e)) => {
            eprintln!("Failed to parse config: {e}");
            #[cfg(target_os = "macos")]
            {
                let msg = format!("Config error: {e}");
                let script = format!(
                    r#"display alert "Captura Configuration Error" message "{msg}" as critical"#
                );
                let _ = std::process::Command::new("osascript")
                    .arg("-e")
                    .arg(&script)
                    .output();
            }
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to load config: {e}");
            process::exit(1);
        }
    };

    // Handle SIGTERM/SIGINT for graceful shutdown
    setup_signal_handlers();

    log::info!("Captura starting...");
    log::info!("Output folder: {}", config.output.folder.display());

    // Run the UI event loop (blocks)
    if let Err(e) = captura_ui::run_event_loop(config) {
        log::error!("UI error: {e}");
        process::exit(1);
    }

    // Clean up lock file
    let _ = std::fs::remove_file(&lock_path);
}

fn lock_file_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("captura")
        .join("captura.lock")
}

fn acquire_lock(path: &PathBuf) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create lock dir: {e}"))?;
    }

    // Check if lock file exists and if the PID is still alive
    if path.exists() {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                // Check if process is still running
                let result = unsafe { libc::kill(pid as i32, 0) };
                if result == 0 {
                    return Err(format!("another instance is running (PID {pid})"));
                }
            }
        }
        // Stale lock file — remove it
        let _ = std::fs::remove_file(path);
    }

    let pid = process::id();
    std::fs::write(path, pid.to_string()).map_err(|e| format!("write lock: {e}"))?;
    Ok(())
}

fn setup_signal_handlers() {
    // Register signal handlers for graceful shutdown
    ctrlc_handler();
}

fn ctrlc_handler() {
    let _ = ctrlc::set_handler(move || {
        log::info!("Received shutdown signal, exiting...");
        let lock_path = lock_file_path();
        let _ = std::fs::remove_file(&lock_path);
        process::exit(0);
    });
}
