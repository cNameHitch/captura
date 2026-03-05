use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use std::collections::HashMap;
use std::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    #[error("Invalid hotkey string: {0}")]
    InvalidHotkey(String),

    #[error("Hotkey already registered: {0}")]
    AlreadyRegistered(String),

    #[error("Hotkey not found: {0}")]
    NotFound(String),

    #[error("Registration failed: {0}")]
    RegistrationFailed(String),

    #[error("Event receiver disconnected")]
    ReceiverDisconnected,
}

pub struct HotkeyManager {
    manager: GlobalHotKeyManager,
    /// Map from our string ID to the registered HotKey
    registered: HashMap<String, HotKey>,
    /// Map from hotkey id (u32) to our string ID
    id_map: HashMap<u32, String>,
    rx: mpsc::Receiver<GlobalHotKeyEvent>,
}

impl HotkeyManager {
    pub fn new() -> Result<Self, HotkeyError> {
        let manager = GlobalHotKeyManager::new()
            .map_err(|e| HotkeyError::RegistrationFailed(format!("init: {e}")))?;

        let (tx, rx) = mpsc::channel();
        // Subscribe to global hotkey events
        std::thread::spawn(move || {
            let receiver = GlobalHotKeyEvent::receiver();
            loop {
                if let Ok(event) = receiver.recv() {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            }
        });

        Ok(Self {
            manager,
            registered: HashMap::new(),
            id_map: HashMap::new(),
            rx,
        })
    }

    /// Register a hotkey. `id` is a caller-defined string identifier.
    pub fn register(&mut self, id: &str, hotkey_str: &str) -> Result<(), HotkeyError> {
        if hotkey_str.is_empty() {
            return Ok(()); // Disabled hotkey
        }

        let hotkey = parse_hotkey(hotkey_str)?;

        self.manager
            .register(hotkey)
            .map_err(|e| HotkeyError::AlreadyRegistered(format!("{hotkey_str}: {e}")))?;

        self.id_map.insert(hotkey.id(), id.to_string());
        self.registered.insert(id.to_string(), hotkey);
        Ok(())
    }

    /// Unregister a previously registered hotkey.
    pub fn unregister(&mut self, id: &str) -> Result<(), HotkeyError> {
        let hotkey = self
            .registered
            .remove(id)
            .ok_or_else(|| HotkeyError::NotFound(id.to_string()))?;

        self.id_map.remove(&hotkey.id());
        self.manager
            .unregister(hotkey)
            .map_err(|e| HotkeyError::RegistrationFailed(format!("unregister: {e}")))?;
        Ok(())
    }

    /// Unregister all hotkeys.
    pub fn unregister_all(&mut self) {
        let ids: Vec<String> = self.registered.keys().cloned().collect();
        for id in ids {
            let _ = self.unregister(&id);
        }
    }

    /// Poll for the next fired hotkey event. Blocks until one fires.
    /// Returns the `id` of the fired hotkey.
    pub fn next_event(&self) -> Result<String, HotkeyError> {
        loop {
            let event = self
                .rx
                .recv()
                .map_err(|_| HotkeyError::ReceiverDisconnected)?;

            if let Some(id) = self.id_map.get(&event.id) {
                return Ok(id.clone());
            }
        }
    }
}

fn parse_hotkey(s: &str) -> Result<HotKey, HotkeyError> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    if parts.is_empty() {
        return Err(HotkeyError::InvalidHotkey(s.to_string()));
    }

    let mut modifiers = Modifiers::empty();
    let key_part = parts.last().unwrap();

    for &part in &parts[..parts.len() - 1] {
        match part.to_lowercase().as_str() {
            "cmd" | "super" | "meta" => modifiers |= Modifiers::SUPER,
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "alt" | "option" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            _ => {
                return Err(HotkeyError::InvalidHotkey(format!(
                    "unknown modifier: {part}"
                )));
            }
        }
    }

    let code = parse_key_code(key_part)
        .ok_or_else(|| HotkeyError::InvalidHotkey(format!("unknown key: {key_part}")))?;

    Ok(HotKey::new(Some(modifiers), code))
}

fn parse_key_code(s: &str) -> Option<Code> {
    match s.to_uppercase().as_str() {
        "A" => Some(Code::KeyA),
        "B" => Some(Code::KeyB),
        "C" => Some(Code::KeyC),
        "D" => Some(Code::KeyD),
        "E" => Some(Code::KeyE),
        "F" => Some(Code::KeyF),
        "G" => Some(Code::KeyG),
        "H" => Some(Code::KeyH),
        "I" => Some(Code::KeyI),
        "J" => Some(Code::KeyJ),
        "K" => Some(Code::KeyK),
        "L" => Some(Code::KeyL),
        "M" => Some(Code::KeyM),
        "N" => Some(Code::KeyN),
        "O" => Some(Code::KeyO),
        "P" => Some(Code::KeyP),
        "Q" => Some(Code::KeyQ),
        "R" => Some(Code::KeyR),
        "S" => Some(Code::KeyS),
        "T" => Some(Code::KeyT),
        "U" => Some(Code::KeyU),
        "V" => Some(Code::KeyV),
        "W" => Some(Code::KeyW),
        "X" => Some(Code::KeyX),
        "Y" => Some(Code::KeyY),
        "Z" => Some(Code::KeyZ),
        "0" => Some(Code::Digit0),
        "1" => Some(Code::Digit1),
        "2" => Some(Code::Digit2),
        "3" => Some(Code::Digit3),
        "4" => Some(Code::Digit4),
        "5" => Some(Code::Digit5),
        "6" => Some(Code::Digit6),
        "7" => Some(Code::Digit7),
        "8" => Some(Code::Digit8),
        "9" => Some(Code::Digit9),
        "F1" => Some(Code::F1),
        "F2" => Some(Code::F2),
        "F3" => Some(Code::F3),
        "F4" => Some(Code::F4),
        "F5" => Some(Code::F5),
        "F6" => Some(Code::F6),
        "F7" => Some(Code::F7),
        "F8" => Some(Code::F8),
        "F9" => Some(Code::F9),
        "F10" => Some(Code::F10),
        "F11" => Some(Code::F11),
        "F12" => Some(Code::F12),
        "F13" => Some(Code::F13),
        "F14" => Some(Code::F14),
        "F15" => Some(Code::F15),
        "F16" => Some(Code::F16),
        "F17" => Some(Code::F17),
        "F18" => Some(Code::F18),
        "F19" => Some(Code::F19),
        "F20" => Some(Code::F20),
        "SPACE" => Some(Code::Space),
        "RETURN" | "ENTER" => Some(Code::Enter),
        _ => None,
    }
}
