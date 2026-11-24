pub mod commands;

use std::sync::Arc;

use brain_llm::LlmBrainManager;
use commands::AppState;
use soul_memory::SoulMemoryStore;
use voice_io::VoicePipeline;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let memory = Arc::new(SoulMemoryStore::new());
    let brain = Arc::new(LlmBrainManager::new());
    let voice = Arc::new(VoicePipeline::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new(memory, brain, voice))
        .invoke_handler(tauri::generate_handler![
            commands::send_chat_message,
            commands::get_ram_status,
            commands::list_models,
            commands::hot_swap_llm,
            commands::switch_asr_model,
            commands::register_custom_model,
            commands::set_voice_mode,
            commands::set_language_pref,
            commands::start_listening,
            commands::stop_listening,
            commands::stop_speaking,
            commands::preview_speech_and_visemes,
            commands::list_memory_facts,
            commands::list_dream_diaries,
            commands::get_relationship_state,
            commands::consolidate_dreams_now,
            commands::export_soul_memory,
            commands::import_soul_memory,
            commands::set_desktop_pet_mode,
            commands::list_live2d_models,
            commands::load_live2d_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running SoulMate2D tauri application");
}
