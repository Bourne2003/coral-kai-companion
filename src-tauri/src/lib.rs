pub mod commands;
pub mod paths;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use brain_llm::LlmBrainManager;
use commands::{AppState, emit_ram_status};
use paths::AppPaths;
use shared_types::{BrainEngine, MemoryEngine, ModelInfo, RamSafetyTier, VoiceEngine};
use soul_memory::SoulMemoryStore;
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tracing::{info, warn};
use voice_io::VoicePipeline;

fn pick_starter_llm(models: &[ModelInfo]) -> Option<ModelInfo> {
    let valid: Vec<&ModelInfo> = models.iter().filter(|m| m.valid).collect();
    if let Some(typhoon) = valid
        .iter()
        .find(|m| m.id.contains("typhoon") || m.path.to_ascii_lowercase().contains("typhoon"))
    {
        return Some((*typhoon).clone());
    }
    if let Some(safe) = valid
        .iter()
        .find(|m| m.safety_tier == RamSafetyTier::Safe8Gb)
    {
        return Some((*safe).clone());
    }
    valid.first().map(|m| (*m).clone())
}

fn pick_starter_asr(models: &[ModelInfo]) -> Option<ModelInfo> {
    let valid: Vec<&ModelInfo> = models.iter().filter(|m| m.valid).collect();
    if let Some(base) = valid
        .iter()
        .find(|m| m.id.contains("base") || m.path.to_ascii_lowercase().contains("ggml-base"))
    {
        return Some((*base).clone());
    }
    valid.first().map(|m| (*m).clone())
}

fn run_graceful_shutdown(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if state.shutdown_done.swap(true, Ordering::AcqRel) {
        return;
    }

    let voice = Arc::clone(&state.voice);
    let memory = Arc::clone(&state.memory);
    let brain = Arc::clone(&state.brain);

    info!("Running SoulMate2D graceful shutdown (consolidating memories & releasing Metal contexts)");
    tauri::async_runtime::block_on(async move {
        let _ = voice.stop_listening().await;
        let _ = voice.stop_speaking().await;
        match memory.consolidate_now().await {
            Ok(Some(diary)) => {
                info!(period = %diary.period_key, "Dream diary consolidated on shutdown");
            }
            Ok(None) => {}
            Err(err) => warn!("Memory consolidation on shutdown failed: {err}"),
        }
        let _ = brain.unload_model().await;
        let _ = voice.unload_asr_model().await;
    });
}

fn spawn_background_model_autoload(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };

        // Load starter Whisper ASR model first (~141MB, fast Metal init).
        match state.voice.scan_asr_models() {
            Ok(asr_models) => {
                if let Some(starter_asr) = pick_starter_asr(&asr_models) {
                    if let Err(err) = state.voice.load_asr_model(&starter_asr).await {
                        warn!(model = %starter_asr.id, "Starter ASR auto-load skipped: {err}");
                    } else {
                        info!(model = %starter_asr.id, "Starter Whisper ASR auto-loaded");
                        emit_ram_status(&app, &state);
                    }
                }
            }
            Err(err) => warn!("ASR scan failed during startup: {err}"),
        }

        // Load starter GGUF LLM on Metal with 8GB RAM guard.
        match state.brain.scan_models() {
            Ok(llm_models) => {
                if let Some(starter_llm) = pick_starter_llm(&llm_models) {
                    if let Err(err) = state.brain.hot_swap(&starter_llm).await {
                        warn!(model = %starter_llm.id, "Starter LLM auto-load skipped: {err}");
                    } else {
                        info!(model = %starter_llm.id, "Starter LLM auto-loaded on Metal");
                    }
                }
            }
            Err(err) => warn!("LLM scan failed during startup: {err}"),
        }

        state.startup_done.store(true, Ordering::Release);
        state.startup_notify.notify_waiters();
        emit_ram_status(&app, &state);
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let paths = AppPaths::resolve(app.handle())?;

            let memory = Arc::new(SoulMemoryStore::open(
                &paths.db_path,
                Some(&paths.bundled_embed_dir),
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

            app.manage(AppState::new(memory, brain, voice, paths));
            spawn_background_model_autoload(app.handle().clone());
            Ok(())
        })
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
        .build(tauri::generate_context!())
        .expect("error while building SoulMate2D tauri application");

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
