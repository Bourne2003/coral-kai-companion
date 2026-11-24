use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::ptr;
use std::sync::Once;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

static WHISPER_LOGGING_INIT: Once = Once::new();

#[repr(C)]
pub struct WhisperLoadResult {
    pub ctx: *mut c_void,
    pub err_ptr: *mut c_char,
}

#[repr(C)]
pub struct WhisperTranscribeResult {
    /// 1 = speech text produced, 0 = empty/silence, -1 = error
    pub status: i32,
    pub text_ptr: *mut c_char,
    pub lang_ptr: *mut c_char,
    pub err_ptr: *mut c_char,
}

fn to_c_ptr(s: &str) -> *mut c_char {
    let sanitized = s.replace('\0', " ");
    CString::new(sanitized)
        .map(CString::into_raw)
        .unwrap_or(ptr::null_mut())
}

unsafe fn free_c_ptr(p: *mut c_char) {
    if !p.is_null() {
        unsafe {
            let _ = CString::from_raw(p);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn soulmate_whisper_abi_version() -> u32 {
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn soulmate_whisper_load(
    model_path_utf8: *const c_char,
    use_gpu: bool,
) -> WhisperLoadResult {
    let res = catch_unwind(AssertUnwindSafe(|| {
        WHISPER_LOGGING_INIT.call_once(|| {
            whisper_rs::install_logging_hooks();
        });

        if model_path_utf8.is_null() {
            return WhisperLoadResult {
                ctx: ptr::null_mut(),
                err_ptr: to_c_ptr("model_path pointer is null"),
            };
        }

        let path_str = match unsafe { CStr::from_ptr(model_path_utf8) }.to_str() {
            Ok(s) => s,
            Err(e) => {
                return WhisperLoadResult {
                    ctx: ptr::null_mut(),
                    err_ptr: to_c_ptr(&format!("Invalid UTF-8 model path: {e}")),
                };
            }
        };

        let mut params = WhisperContextParameters::default();
        params.use_gpu(use_gpu).flash_attn(false);

        match WhisperContext::new_with_params(Path::new(path_str), params) {
            Ok(ctx) => WhisperLoadResult {
                ctx: Box::into_raw(Box::new(ctx)) as *mut c_void,
                err_ptr: ptr::null_mut(),
            },
            Err(e) => WhisperLoadResult {
                ctx: ptr::null_mut(),
                err_ptr: to_c_ptr(&format!("Failed to initialize Whisper Metal: {e}")),
            },
        }
    }));

    res.unwrap_or_else(|_| WhisperLoadResult {
        ctx: ptr::null_mut(),
        err_ptr: to_c_ptr("Whisper bridge panicked while loading model"),
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn soulmate_whisper_free_load_error(err_ptr: *mut c_char) {
    unsafe { free_c_ptr(err_ptr) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn soulmate_whisper_free(ctx: *mut c_void) {
    if !ctx.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            let _ = Box::from_raw(ctx as *mut WhisperContext);
        }));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn soulmate_whisper_transcribe(
    ctx: *const c_void,
    pcm_ptr: *const f32,
    pcm_len: usize,
    lang_code: u8,
) -> WhisperTranscribeResult {
    let res = catch_unwind(AssertUnwindSafe(|| {
        if ctx.is_null() {
            return WhisperTranscribeResult {
                status: -1,
                text_ptr: ptr::null_mut(),
                lang_ptr: ptr::null_mut(),
                err_ptr: to_c_ptr("Whisper context pointer is null"),
            };
        }
        if pcm_len == 0 || pcm_ptr.is_null() {
            return WhisperTranscribeResult {
                status: 0,
                text_ptr: ptr::null_mut(),
                lang_ptr: ptr::null_mut(),
                err_ptr: ptr::null_mut(),
            };
        }

        let ctx_ref = unsafe { &*(ctx as *const WhisperContext) };
        let pcm = unsafe { std::slice::from_raw_parts(pcm_ptr, pcm_len) };

        let mut state = match ctx_ref.create_state() {
            Ok(s) => s,
            Err(e) => {
                return WhisperTranscribeResult {
                    status: -1,
                    text_ptr: ptr::null_mut(),
                    lang_ptr: ptr::null_mut(),
                    err_ptr: to_c_ptr(&format!("Failed to create Whisper state: {e}")),
                };
            }
        };

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().clamp(1, 4))
            .unwrap_or(4) as std::ffi::c_int;
        params.set_n_threads(threads);
        params.set_translate(false);
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_detect_language(false);

        match lang_code {
            1 => params.set_language(Some("th")),
            2 => params.set_language(Some("en")),
            _ => params.set_language(None),
        }

        if let Err(e) = state.full(params, pcm) {
            return WhisperTranscribeResult {
                status: -1,
                text_ptr: ptr::null_mut(),
                lang_ptr: ptr::null_mut(),
                err_ptr: to_c_ptr(&format!("Whisper inference failed: {e}")),
            };
        }

        let mut text = String::new();
        for segment in state.as_iter() {
            let seg = match segment.to_str_lossy() {
                Ok(s) => s,
                Err(e) => {
                    return WhisperTranscribeResult {
                        status: -1,
                        text_ptr: ptr::null_mut(),
                        lang_ptr: ptr::null_mut(),
                        err_ptr: to_c_ptr(&format!("Whisper segment decode failed: {e}")),
                    };
                }
            };
            let cleaned = clean_whisper_segment(&seg);
            if !cleaned.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(cleaned);
            }
        }

        let trimmed = text.trim();
        if trimmed.is_empty() {
            return WhisperTranscribeResult {
                status: 0,
                text_ptr: ptr::null_mut(),
                lang_ptr: ptr::null_mut(),
                err_ptr: ptr::null_mut(),
            };
        }

        let language = match lang_code {
            1 => "th".to_string(),
            2 => "en".to_string(),
            _ => {
                if trimmed.chars().any(|c| ('\u{0E00}'..='\u{0E7F}').contains(&c)) {
                    "th".to_string()
                } else {
                    whisper_rs::get_lang_str(state.full_lang_id_from_state())
                        .unwrap_or("en")
                        .to_string()
                }
            }
        };

        WhisperTranscribeResult {
            status: 1,
            text_ptr: to_c_ptr(trimmed),
            lang_ptr: to_c_ptr(&language),
            err_ptr: ptr::null_mut(),
        }
    }));

    res.unwrap_or_else(|_| WhisperTranscribeResult {
        status: -1,
        text_ptr: ptr::null_mut(),
        lang_ptr: ptr::null_mut(),
        err_ptr: to_c_ptr("Whisper bridge panicked during transcription"),
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn soulmate_whisper_free_result(res: WhisperTranscribeResult) {
    unsafe {
        free_c_ptr(res.text_ptr);
        free_c_ptr(res.lang_ptr);
        free_c_ptr(res.err_ptr);
    }
}

fn clean_whisper_segment(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "";
    }
    let upper = trimmed.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "[BLANK_AUDIO]" | "(SILENCE)" | "[SILENCE]" | "[MUSIC]" | "(MUSIC)" | "[NOISE]"
    ) {
        return "";
    }
    trimmed
}
