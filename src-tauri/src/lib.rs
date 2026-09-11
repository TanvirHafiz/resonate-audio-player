mod api;
mod db;
mod history;
mod ingest;
mod models;
mod playback;
#[cfg(feature = "pro")]
mod pro;
mod streaming;

use db::Db;
use playback::Player;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("failed to resolve app data dir");
            let conn = db::init(&app_data_dir);

            #[cfg(feature = "pro")]
            {
                let queue = pro::worker::FeatureQueue::spawn(app_data_dir.clone());
                queue.enqueue_all(pro::worker::find_unanalyzed_tracks(&conn));
                app.manage(queue);
            }

            app.manage(Db(std::sync::Mutex::new(conn)));
            app.manage(Player::new());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            api::scan_folder,
            api::add_files,
            api::remove_tracks,
            api::add_stream,
            api::fetch_podcast_feed,
            api::get_library_folders,
            api::get_tracks,
            api::play_track,
            api::toggle_play_pause,
            api::stop_playback,
            api::next_track,
            api::previous_track,
            api::set_volume,
            api::seek_to,
            api::get_now_playing,
            api::debug_log,
            api::app_tier,
            #[cfg(feature = "pro")]
            pro::api::more_like_this,
            #[cfg(feature = "pro")]
            pro::api::for_you_queue,
            #[cfg(feature = "pro")]
            pro::api::get_all_track_tags,
            #[cfg(feature = "pro")]
            pro::api::is_track_analyzed,
            #[cfg(feature = "pro")]
            pro::api::requeue_unanalyzed_tracks,
            #[cfg(feature = "pro")]
            pro::api::create_playlist,
            #[cfg(feature = "pro")]
            pro::api::rename_playlist,
            #[cfg(feature = "pro")]
            pro::api::delete_playlist,
            #[cfg(feature = "pro")]
            pro::api::list_playlists,
            #[cfg(feature = "pro")]
            pro::api::get_playlist_tracks,
            #[cfg(feature = "pro")]
            pro::api::add_track_to_playlist,
            #[cfg(feature = "pro")]
            pro::api::remove_track_from_playlist,
            #[cfg(feature = "pro")]
            pro::api::remove_library_folder,
            #[cfg(feature = "pro")]
            pro::api::list_output_devices,
            #[cfg(feature = "pro")]
            pro::api::get_current_output_device,
            #[cfg(feature = "pro")]
            pro::api::set_output_device,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let db = app_handle.state::<Db>();
                let player = app_handle.state::<Player>();
                let _ = api::record_history_for_current(&db, &player);
            }
        });
}
