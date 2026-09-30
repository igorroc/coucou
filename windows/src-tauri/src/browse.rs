// Native "open file" dialog — the click-to-browse fallback for the drop zone.
// Drag & drop never survived WebView2 on this machine, so clicking the zone
// opens the real Explorer picker instead and the picked path flows through the
// usual inbox ingest, with the same swallow choreography as a drop.
//
// No new dependencies: IFileOpenDialog comes from the `windows` crate we
// already use. The dialog must run on the main (STA) thread, so the command
// below marshals the call there and waits on a channel.

use std::path::PathBuf;

use tauri::AppHandle;
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
use windows::core::{w, HRESULT};

/// Shows the picker on the main thread. `Ok(None)` = the user cancelled.
pub fn pick_file(app: &AppHandle) -> Result<Option<PathBuf>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(show_dialog());
    })
    .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| format!("picker failed: {e}"))?
}

fn show_dialog() -> Result<Option<PathBuf>, String> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
        dialog
            .SetTitle(w!("Choose a file for Mochi"))
            .map_err(|e| e.to_string())?;

        match dialog.Show(None) {
            Ok(()) => {}
            // Cancel is a normal outcome, not an error — the island just stays put.
            Err(e) if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => return Ok(None),
            Err(e) => return Err(format!("picker failed: {e}")),
        }

        let item = dialog.GetResult().map_err(|e| e.to_string())?;
        let name = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|e| e.to_string())?;
        let path = name.to_string().map_err(|e| e.to_string())?;
        Ok(Some(PathBuf::from(path)))
    }
}
