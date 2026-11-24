pub mod avatar_state;
pub mod commands;
pub mod config;
pub mod downloader;
pub mod inner_loop;
pub mod paths;

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use avatar_state::AvatarStateServer;
use brain_llm::{CharacterPersona, LlmBrainManager, McpClientManager, McpPluginBridge, PluginRegistry};
use commands::{AppState, build_custom_embed_info};
use config::ModelsConfig;
use inner_loop::InnerLoopManager;
use paths::{AppPaths, inspect_live2d_folder};
use shared_types::{BrainEngine, CharacterId, MemoryEngine, ModelInfo, ModelSource, VoiceEngine};
use soul_memory::SoulMemoryStore;
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tracing::{info, warn};
use voice_io::VoicePipeline;

fn run_graceful_shutdown(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if state.shutdown_done.swap(true, Ordering::AcqRel) {
        return;
    }

    let voice = Arc::clone(&state.voice);
    let coral_memory = Arc::clone(&state.coral_memory);
    let kai_memory = Arc::clone(&state.kai_memory);
    let brain = Arc::clone(&state.brain);

    info!("Running Coral&Kai Companion graceful shutdown (consolidating memories & releasing Metal contexts)");
    tauri::async_runtime::block_on(async move {
        let _ = voice.stop_listening().await;
        let _ = voice.stop_speaking().await;
        for (label, memory) in [("coral", coral_memory), ("kai", kai_memory)] {
            match memory.consolidate_now().await {
                Ok(Some(diary)) => {
                    info!(character = label, period = %diary.period_key, "Dream diary consolidated on shutdown");
                }
                Ok(None) => {}
                Err(err) => warn!(character = label, "Memory consolidation on shutdown failed: {err}"),
            }
        }
        let _ = brain.unload_model().await;
        let _ = voice.unload_asr_model().await;
    });
}

fn restore_saved_embed_models(config: &ModelsConfig) -> Vec<ModelInfo> {
    let mut restored = Vec::new();
    for entry in &config.embed {
        if let Ok(info) =
            build_custom_embed_info(Path::new(&entry.path), Some(&entry.name), Some(&entry.id))
        {
            restored.push(info);
        }
    }
    restored
}

fn restore_saved_live2d_models(app: &AppHandle, config: &ModelsConfig) -> Vec<ModelInfo> {
    let mut restored = Vec::new();
    for entry in &config.live2d {
        if let Ok((canon_folder, mut info)) =
            inspect_live2d_folder(Path::new(&entry.path), ModelSource::Custom, None)
        {
            if !entry.name.trim().is_empty() {
                info.name = entry.name.clone();
            }
            let _ = app
                .asset_protocol_scope()
                .allow_directory(&canon_folder, true);
            restored.push(info);
        }
    }
    restored
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let paths = AppPaths::resolve(app.handle())?;

            // BYOM zero-model boot: isolated per-character SQLite stores open immediately
            // in char-ngram fallback mode, and neither LLM nor Whisper weights load at startup.
            let coral_memory = Arc::new(SoulMemoryStore::open_for_character(
                &paths.coral_db_path,
                None,
                CharacterId::Coral,
            )?);
            let kai_memory = Arc::new(SoulMemoryStore::open_for_character(
                &paths.kai_db_path,
                None,
                CharacterId::Kai,
            )?);

            let brain = Arc::new(LlmBrainManager::with_dirs(
                &paths.bundled_llm_dir,
                &paths.user_llm_dir,
            ));
            let voice = Arc::new(VoicePipeline::with_asr_dirs(
                Some(paths.bundled_asr_dir.clone()),
                Some(paths.user_asr_dir.clone()),
            ));

            let _ = app
                .asset_protocol_scope()
                .allow_directory(&paths.bundled_live2d_dir, true);
            let _ = app
                .asset_protocol_scope()
                .allow_directory(&paths.user_live2d_dir, true);

            let models_config = ModelsConfig::load(&paths.config_path);
            let _ = brain.restore_custom_models(&ModelsConfig::as_restore_tuples(&models_config.llm));
            let _ = voice.restore_custom_models(&ModelsConfig::as_restore_tuples(&models_config.asr));
            let restored_embed = restore_saved_embed_models(&models_config);
            let restored_live2d = restore_saved_live2d_models(app.handle(), &models_config);

            let active_character_id = CharacterId::from_str(&models_config.active_character)
                .unwrap_or(CharacterId::Coral);
            let persona = match active_character_id {
                CharacterId::Kai => CharacterPersona::Kai,
                CharacterId::Coral => CharacterPersona::Coral,
            };
            brain.set_persona(persona);
            voice.set_voice_persona(persona == CharacterPersona::Kai);
            let active_live2d_id = match persona {
                CharacterPersona::Kai => "kai_companion",
                CharacterPersona::Coral => "coral_companion",
            }
            .to_string();

            let initial_profile = match active_character_id {
                CharacterId::Coral => coral_memory.get_emotion_profile()?,
                CharacterId::Kai => kai_memory.get_emotion_profile()?,
            };
            let avatar_state = Arc::new(AvatarStateServer::new(active_character_id, initial_profile));
            let inner_loop = Arc::new(InnerLoopManager::new(models_config.inner_loop.clone()));

            let mcp_manager = Arc::new(McpClientManager::new(models_config.mcp.clone()));
            let plugin_registry = Arc::new(PluginRegistry::new_with_builtins());
            plugin_registry.register(Arc::new(McpPluginBridge::new(Arc::clone(&mcp_manager))));

            // Initialize external MCP tool discovery asynchronously ONLY when explicitly enabled.
            if models_config.mcp.enabled {
                let mcp_bg = Arc::clone(&mcp_manager);
                tauri::async_runtime::spawn(async move {
                    let _ = mcp_bg.refresh_tools().await;
                });
            }

            let app_state = AppState::new(
                Arc::clone(&coral_memory),
                Arc::clone(&kai_memory),
                active_character_id,
                Arc::clone(&brain),
                Arc::clone(&voice),
                Arc::clone(&avatar_state),
                Arc::clone(&inner_loop),
                Arc::clone(&plugin_registry),
                mcp_manager,
                paths,
                models_config,
                restored_embed,
                restored_live2d,
                active_live2d_id,
            );
            let active_character_arc = Arc::clone(&app_state.active_character);
            let shutdown_done = Arc::clone(&app_state.shutdown_done);
            app.manage(app_state);

            InnerLoopManager::spawn_background_loops(
                app.handle().clone(),
                shutdown_done,
                coral_memory,
                kai_memory,
                active_character_arc,
                brain,
                voice,
                avatar_state,
                inner_loop,
                plugin_registry,
            );

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::send_chat_message,
            commands::get_ram_status,
            commands::list_models,
            commands::hot_swap_llm,
            commands::switch_asr_model,
            commands::switch_embed_model,
            commands::register_custom_model,
            commands::remove_custom_model,
            commands::set_character_persona,
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
            commands::get_emotion_profile,
            commands::list_diary_entries,
            commands::add_diary_entry,
            commands::get_persona_bio,
            commands::update_persona_bio,
            commands::get_user_profile,
            commands::update_user_profile,
            commands::get_avatar_state,
            commands::set_avatar_logical_state,
            commands::get_inner_loop_status,
            commands::update_inner_loop_config,
            commands::trigger_inner_loop_beat,
            commands::list_plugins,
            commands::set_plugin_enabled,
            commands::execute_plugin_action,
            commands::get_mcp_config,
            commands::update_mcp_config,
            commands::list_mcp_tools,
            commands::list_starter_catalog,
            commands::download_starter_asset,
            commands::cancel_download,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Coral&Kai Companion tauri application");

    app.run(|app_handle, event| match event {
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { .. },
            ..
        } if label == "main" => {
            run_graceful_shutdown(app_handle);
        }
        RunEvent::ExitRequested { .. } | RunEvent::Exit => {
            run_graceful_shutdown(app_handle);
        }
        _ => {}
    });
}
