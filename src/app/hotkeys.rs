use crossbeam_channel::Sender;
use eframe::egui;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub enum HotkeyAction {
    StartStop,
    Pause,
    Marker,
}

pub const HINT: &str = "Ctrl+Alt+R record/stop · Ctrl+Alt+P pause · Ctrl+Alt+M marker";

pub struct Hotkeys {
    _mgr: GlobalHotKeyManager,
}

pub fn setup(ctx: egui::Context, tx: Sender<HotkeyAction>) -> anyhow::Result<Hotkeys> {
    let mgr = GlobalHotKeyManager::new()?;
    let mods = Modifiers::CONTROL | Modifiers::ALT;
    let keys = [
        (HotKey::new(Some(mods), Code::KeyR), HotkeyAction::StartStop),
        (HotKey::new(Some(mods), Code::KeyP), HotkeyAction::Pause),
        (HotKey::new(Some(mods), Code::KeyM), HotkeyAction::Marker),
    ];
    let mut map = HashMap::new();
    for (hk, action) in keys {
        map.insert(hk.id(), action);
        mgr.register(hk)?;
    }
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.state == HotKeyState::Pressed {
            if let Some(a) = map.get(&e.id) {
                let _ = tx.send(*a);
                ctx.request_repaint();
            }
        }
    }));
    Ok(Hotkeys { _mgr: mgr })
}
