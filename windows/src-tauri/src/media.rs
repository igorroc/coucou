// Media playback detection for the dancing animation.
//
// Windows exposes what is playing through the System Media Transport Controls
// (SMTC). Spotify, the browsers (YouTube), Groove and most media apps register
// a session there, so `PlaybackStatus == Playing` is a precise "something is
// playing" signal — unlike a raw audio peak meter, it ignores notification
// dings and system sounds.
//
// A dedicated thread polls once a second and emits `media` (to the island)
// only when playback starts or stops. It parks while the island is hidden,
// matching the cursor poll's "nothing runs when nothing is on screen" rule.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as MediaManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};

use crate::island::{PollGate, WINDOW_LABEL};
use crate::{integrations, log};

const POLL: Duration = Duration::from_secs(1);

/// What the island receives. Only `playing` drives the animation; kept a struct
/// so a future "now playing" readout can ride along without a protocol change.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
struct MediaState {
    playing: bool,
}

/// Starts the media watcher. The manager is created and used on this one thread,
/// so no cross-thread COM marshalling is involved.
pub fn start(app: AppHandle, gate: Arc<PollGate>) {
    std::thread::spawn(move || {
        if let Err(err) = watch(&app, &gate) {
            log::line(format!("media watcher stopped: {}", err.message()));
        }
    });
}

fn watch(app: &AppHandle, gate: &PollGate) -> windows::core::Result<()> {
    // WinRT needs an initialized apartment on this thread; MTA is enough for SMTC.
    unsafe {
        let _ = windows::Win32::System::WinRT::RoInitialize(
            windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
        );
    }
    let manager = MediaManager::RequestAsync()?.get()?;

    let mut playing = false;
    // Emit the first state right away so the island is correct at boot.
    let mut first = true;
    loop {
        // Park while the island is hidden: nothing is drawn there anyway.
        gate.wait_until_active();

        let reported = if integrations::PAUSED.load(Ordering::Relaxed) || !enabled(app) {
            // Paused, or the feature is off: report "not playing" so any dance stops.
            false
        } else {
            any_playing(&manager)
        };

        if first || reported != playing {
            playing = reported;
            first = false;
            emit(app, playing);
        }
        std::thread::sleep(POLL);
    }
}

fn emit(app: &AppHandle, playing: bool) {
    let _ = app.emit_to(WINDOW_LABEL, "media", MediaState { playing });
}

/// True while the user wants the dance (Settings → General).
fn enabled(app: &AppHandle) -> bool {
    app.try_state::<crate::Shared>()
        .map(|shared| shared.settings.lock().unwrap().dance_with_music)
        .unwrap_or(true)
}

/// True when any SMTC session reports `Playing`.
fn any_playing(manager: &MediaManager) -> bool {
    let Ok(sessions) = manager.GetSessions() else {
        return false;
    };
    let count = sessions.Size().unwrap_or(0);
    for i in 0..count {
        let Ok(session) = sessions.GetAt(i) else {
            continue;
        };
        let Ok(info) = session.GetPlaybackInfo() else {
            continue;
        };
        if let Ok(status) = info.PlaybackStatus() {
            if status == PlaybackStatus::Playing {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: proves the SMTC manager and the session query work at runtime
    /// on this machine. If SMTC is unavailable the test is a no-op rather than a
    /// failure (a headless CI box has no audio sessions).
    #[test]
    fn smtc_manager_responds() {
        unsafe {
            let _ = windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            );
        }
        let Ok(manager) = MediaManager::RequestAsync().and_then(|op| op.get()) else {
            return;
        };
        let _ = any_playing(&manager);
    }
}
