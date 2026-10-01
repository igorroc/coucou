// Navi Assistant runs without a console window: Navi is the whole UI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    navi_assistant_lib::run()
}
