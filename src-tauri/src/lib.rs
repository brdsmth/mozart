mod backend;
mod commands;
mod db;
mod models;
mod tmux;

use commands::{Db, RunRegistry};
use std::collections::HashMap;
use std::sync::Mutex;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Db(Mutex::new(db::open())))
        .manage(RunRegistry(Mutex::new(HashMap::new())))
        .setup(|app| {
            // Runs left `status = 'running'` from before this launch (Tauri
            // crashed or was closed mid-turn) — reconnect to their tmux
            // sessions instead of leaving them stuck forever. Spawned so
            // startup doesn't block on it.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                commands::reconcile_startup_runs(&handle).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::create_task,
            commands::list_tasks,
            commands::list_messages,
            commands::send_message,
            commands::cancel_run,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
