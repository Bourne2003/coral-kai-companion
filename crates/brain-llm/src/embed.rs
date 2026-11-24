use shared_types::{SoulError, SoulResult};

/// Embedding dimensionality matching `sqlite-vec` `vec_facts` / `vec_episodes` tables (`float[384]`).
pub const LOCAL_EMBEDDING_DIM: usize = 384;

/// Computes a deterministic, 100% offline 384-dim L2-normalized embedding using
/// Unicode character 2/3-grams and word feature hashing (supporting both Thai and English).
pub fn compute_local_embedding(text: &str) -> SoulResult<Vec<f32>> {
    let normalized: String = text
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphabetic() {
                c.to_ascii_lowercase()
            } else {
                c
            }
        })
        .collect();

    if normalized.is_empty() {
        return Err(SoulError::InvalidInput(
            "Cannot compute embedding for empty text".to_string(),
        ));
    }

    let mut vec = vec![0.0_f32; LOCAL_EMBEDDING_DIM];
    let chars: Vec<char> = normalized.chars().collect();

    for &ch in &chars {
        if !ch.is_whitespace() {
            hash_feature(&mut vec, &[ch as u32], 0.35);
        }
    }

    for win in chars.windows(2) {
        hash_feature(&mut vec, &[win[0] as u32, win[1] as u32], 0.75);
    }

    for win in chars.windows(3) {
        hash_feature(
            &mut vec,
            &[win[0] as u32, win[1] as u32, win[2] as u32],
            1.0,
        );
    }

    for word in normalized.split_whitespace() {
        let codes: Vec<u32> = word.chars().map(|c| c as u32).collect();
        hash_feature(&mut vec, &codes, 1.4);
    }

    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for v in &mut vec {
            *v /= norm;
        }
    }

    Ok(vec)
}

fn hash_feature(vec: &mut [f32], seq: &[u32], weight: f32) {
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    for &item in seq {
        h1 ^= u64::from(item);
        h1 = h1.wrapping_mul(0x0100_0000_01b3);
    }
    let idx = (h1 as usize) % vec.len();
    let sign = if ((h1 >> 63) & 1) == 0 { 1.0 } else { -1.0 };
    vec[idx] += sign * weight;
}
