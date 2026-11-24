use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, params};
use shared_types::{
    DreamDiary, EpisodeRecord, MemoryFact, MemoryRetrievalContext, RelationshipState, SoulResult,
    now_timestamp_ms,
};

use crate::embedder::{Embedder, EMBEDDING_DIM, embedding_to_le_bytes, normalize_embedding};
use crate::facts::row_to_fact;
use crate::schema::db_err;

const RRF_K: f64 = 60.0;
const WEIGHT_VEC: f64 = 2.5;
const WEIGHT_FTS: f64 = 2.0;
const WEIGHT_HEAT: f64 = 1.0;
const WEIGHT_ACTR: f64 = 1.0;
const ACTR_DECAY_D: f64 = 0.5;
const MIN_VEC_COSINE_SIM: f64 = 0.15;

/// Retrieve hybrid-ranked facts, episodes, recent dream diaries, and current relationship state.
pub fn retrieve_context_sync(
    conn: &mut Connection,
    embedder: &Embedder,
    query: &str,
    query_embedding: Option<&[f32]>,
    fact_limit: usize,
    episode_limit: usize,
) -> SoulResult<MemoryRetrievalContext> {
    let now_ms = now_timestamp_ms();
    let trimmed_query = query.trim();

    let resolved_query_vec = resolve_query_embedding(embedder, trimmed_query, query_embedding)?;

    let facts = retrieve_facts(
        conn,
        trimmed_query,
        resolved_query_vec.as_deref(),
        fact_limit,
        now_ms,
    )?;

    let episodes = retrieve_episodes(
        conn,
        trimmed_query,
        resolved_query_vec.as_deref(),
        episode_limit,
        now_ms,
    )?;

    let recent_diaries = list_dream_diaries_query(conn, 5)?;
    let relationship = get_relationship_state_query(conn)?;

    Ok(MemoryRetrievalContext {
        facts,
        episodes,
        recent_diaries,
        relationship,
    })
}

fn resolve_query_embedding(
    embedder: &Embedder,
    query: &str,
    query_embedding: Option<&[f32]>,
) -> SoulResult<Option<Vec<f32>>> {
    if let Some(raw) = query_embedding {
        if raw.len() == EMBEDDING_DIM {
            let mut owned = raw.to_vec();
            if normalize_embedding(&mut owned) {
                return Ok(Some(owned));
            }
        }
    }

    if query.is_empty() {
        return Ok(None);
    }

    Ok(Some(embedder.embed(query)?))
}

fn retrieve_facts(
    conn: &mut Connection,
    query: &str,
    query_vec: Option<&[f32]>,
    limit: usize,
    now_ms: i64,
) -> SoulResult<Vec<MemoryFact>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    // Load active facts (superseded rows strictly excluded).
    let mut active_stmt = conn
        .prepare(
            "SELECT id, category, key, value, confidence, importance, memory_heat,
                    access_count, created_at, last_accessed_at, superseded_by, superseded_at
             FROM facts
             WHERE superseded_by IS NULL",
        )
        .map_err(db_err)?;
    let active_rows = active_stmt.query_map([], row_to_fact).map_err(db_err)?;
    let mut facts_by_id: HashMap<String, MemoryFact> = HashMap::new();
    for item in active_rows {
        let fact = item.map_err(db_err)?;
        facts_by_id.insert(fact.id.clone(), fact);
    }
    drop(active_stmt);

    if facts_by_id.is_empty() {
        return Ok(Vec::new());
    }

    // 1. Vector cosine similarity candidates via sqlite-vec.
    let mut vec_scores: Vec<(String, f64)> = Vec::new();
    if let Some(qvec) = query_vec {
        let k = (limit.saturating_mul(8)).max(64).min(facts_by_id.len());
        let qbytes = embedding_to_le_bytes(qvec);
        let mut stmt = conn
            .prepare(
                "SELECT fact_id, distance
                 FROM vec_facts
                 WHERE embedding MATCH ?1 AND k = ?2",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![&qbytes, k as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(db_err)?;
        for item in rows {
            let (id, distance) = item.map_err(db_err)?;
            if facts_by_id.contains_key(&id) {
                let cosine_sim = 1.0 - distance;
                vec_scores.push((id, cosine_sim));
            }
        }
    }

    // Apply similarity relevance gate if at least one candidate is genuinely similar,
    // preventing orthogonal memories from receiving vector RRF credit.
    let has_strong_vec_match = vec_scores.iter().any(|(_, sim)| *sim >= MIN_VEC_COSINE_SIM);
    if has_strong_vec_match {
        vec_scores.retain(|(_, sim)| *sim >= MIN_VEC_COSINE_SIM);
    }
    let vec_ranks = dense_ranks_desc(&vec_scores, 1e-3);

    // 2. FTS5 BM25 keyword scores (with Thai shingle fallback if query has Thai and FTS5 returns 0).
    let mut fts_scores: Vec<(String, f64)> = Vec::new();
    if !query.is_empty() {
        if let Some(fts_query) = build_fts5_query(query) {
            let fts_limit = (limit.saturating_mul(8)).max(64) as i64;
            let mut stmt = conn
                .prepare(
                    "SELECT f.id, -bm25(facts_fts) AS pos_bm25
                     FROM facts_fts
                     JOIN facts f ON f.id = facts_fts.fact_id
                     WHERE facts_fts MATCH ?1 AND f.superseded_by IS NULL
                     ORDER BY bm25(facts_fts) ASC
                     LIMIT ?2",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![&fts_query, fts_limit], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })
                .map_err(db_err)?;
            for item in rows {
                fts_scores.push(item.map_err(db_err)?);
            }
        }

        if fts_scores.is_empty() && contains_thai(query) {
            for (id, fact) in &facts_by_id {
                let hay = format!("{} {} {}", fact.category, fact.key, fact.value);
                let overlap = thai_shingle_overlap(query, &hay);
                if overlap > 0.0 {
                    fts_scores.push((id.clone(), overlap));
                }
            }
        }
    }
    let fts_ranks = dense_ranks_desc(&fts_scores, 1e-4);

    // 3. Ebbinghaus decay heat and 4. ACT-R base-level activation across active facts.
    let mut heat_scores: Vec<(String, f64)> = Vec::with_capacity(facts_by_id.len());
    let mut actr_scores: Vec<(String, f64)> = Vec::with_capacity(facts_by_id.len());
    let mut metrics_by_id: HashMap<String, (f64, f64)> = HashMap::with_capacity(facts_by_id.len());

    for (id, fact) in &facts_by_id {
        let dt_hours = ((now_ms - fact.last_accessed_at).max(0) as f64) / 3_600_000.0;
        let stability_hours =
            24.0 * (1.0 + f64::from(fact.importance).max(0.0) * 4.0 + f64::from(fact.access_count) * 0.5);
        let decayed_heat = f64::from(fact.memory_heat).max(0.0) * (-dt_hours / stability_hours).exp();
        let actr = (1.0 + f64::from(fact.access_count)).ln() - ACTR_DECAY_D * (1.0 + dt_hours).ln();

        heat_scores.push((id.clone(), decayed_heat));
        actr_scores.push((id.clone(), actr));
        metrics_by_id.insert(id.clone(), (decayed_heat, actr));
    }

    let heat_ranks = dense_ranks_desc(&heat_scores, 1e-6);
    let actr_ranks = dense_ranks_desc(&actr_scores, 1e-6);

    // Reciprocal Rank Fusion combination.
    let mut scored: Vec<(String, f64, f64, f64, f32, i64)> = Vec::with_capacity(facts_by_id.len());
    for (id, fact) in &facts_by_id {
        let mut rrf = 0.0_f64;
        if let Some(&r) = vec_ranks.get(id) {
            rrf += WEIGHT_VEC * (1.0 / (RRF_K + r as f64));
        }
        if let Some(&r) = fts_ranks.get(id) {
            rrf += WEIGHT_FTS * (1.0 / (RRF_K + r as f64));
        }
        if let Some(&r) = heat_ranks.get(id) {
            rrf += WEIGHT_HEAT * (1.0 / (RRF_K + r as f64));
        }
        if let Some(&r) = actr_ranks.get(id) {
            rrf += WEIGHT_ACTR * (1.0 / (RRF_K + r as f64));
        }

        let (decayed_heat, actr) = metrics_by_id.get(id).copied().unwrap_or((0.0, 0.0));
        scored.push((
            id.clone(),
            rrf,
            decayed_heat,
            actr,
            fact.importance,
            fact.last_accessed_at,
        ));
    }

    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| b.5.cmp(&a.5))
            .then_with(|| a.0.cmp(&b.0))
    });

    let selected_ids: Vec<String> = scored.into_iter().take(limit).map(|(id, ..)| id).collect();
    if selected_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Update access_count += 1, last_accessed_at = now, and boost memory_heat for retrieved facts.
    let mut results = Vec::with_capacity(selected_ids.len());
    let tx = conn.transaction().map_err(db_err)?;
    for id in selected_ids {
        if let Some(mut fact) = facts_by_id.remove(&id) {
            let (decayed_heat, _) = metrics_by_id.get(&id).copied().unwrap_or((0.0, 0.0));
            let boosted_heat = compute_rehearsal_heat(fact.memory_heat, decayed_heat);
            fact.access_count = fact.access_count.saturating_add(1);
            fact.last_accessed_at = now_ms;
            fact.memory_heat = boosted_heat;

            tx.execute(
                "UPDATE facts
                 SET access_count = ?1,
                     last_accessed_at = ?2,
                     memory_heat = ?3
                 WHERE id = ?4",
                params![fact.access_count, fact.last_accessed_at, fact.memory_heat, &fact.id],
            )
            .map_err(db_err)?;
            results.push(fact);
        }
    }
    tx.commit().map_err(db_err)?;

    Ok(results)
}

fn retrieve_episodes(
    conn: &mut Connection,
    query: &str,
    query_vec: Option<&[f32]>,
    limit: usize,
    now_ms: i64,
) -> SoulResult<Vec<EpisodeRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare(
            "SELECT id, summary, raw_turn_count, emotional_valence, memory_heat,
                    access_count, created_at, last_accessed_at
             FROM episodes",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                EpisodeRecord {
                    id: row.get(0)?,
                    summary: row.get(1)?,
                    raw_turn_count: row.get(2)?,
                    emotional_valence: row.get(3)?,
                    memory_heat: row.get(4)?,
                    created_at: row.get(6)?,
                    last_accessed_at: row.get(7)?,
                },
                row.get::<_, u32>(5)?,
            ))
        })
        .map_err(db_err)?;

    let mut episodes_by_id: HashMap<String, (EpisodeRecord, u32)> = HashMap::new();
    for item in rows {
        let (ep, access_count) = item.map_err(db_err)?;
        episodes_by_id.insert(ep.id.clone(), (ep, access_count));
    }
    drop(stmt);

    if episodes_by_id.is_empty() {
        return Ok(Vec::new());
    }

    let mut vec_scores: Vec<(String, f64)> = Vec::new();
    if let Some(qvec) = query_vec {
        let k = (limit.saturating_mul(8)).max(32).min(episodes_by_id.len());
        let qbytes = embedding_to_le_bytes(qvec);
        let mut vstmt = conn
            .prepare(
                "SELECT episode_id, distance
                 FROM vec_episodes
                 WHERE embedding MATCH ?1 AND k = ?2",
            )
            .map_err(db_err)?;
        let vrows = vstmt
            .query_map(params![&qbytes, k as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(db_err)?;
        for item in vrows {
            let (id, distance) = item.map_err(db_err)?;
            if episodes_by_id.contains_key(&id) {
                vec_scores.push((id, 1.0 - distance));
            }
        }
    }
    if vec_scores.iter().any(|(_, sim)| *sim >= MIN_VEC_COSINE_SIM) {
        vec_scores.retain(|(_, sim)| *sim >= MIN_VEC_COSINE_SIM);
    }
    let vec_ranks = dense_ranks_desc(&vec_scores, 1e-3);

    let mut fts_scores: Vec<(String, f64)> = Vec::new();
    if !query.is_empty() {
        if let Some(fts_query) = build_fts5_query(query) {
            let fts_limit = (limit.saturating_mul(8)).max(32) as i64;
            let mut fstmt = conn
                .prepare(
                    "SELECT episode_id, -bm25(episodes_fts) AS pos_bm25
                     FROM episodes_fts
                     WHERE episodes_fts MATCH ?1
                     ORDER BY bm25(episodes_fts) ASC
                     LIMIT ?2",
                )
                .map_err(db_err)?;
            let frows = fstmt
                .query_map(params![&fts_query, fts_limit], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })
                .map_err(db_err)?;
            for item in frows {
                let (id, score) = item.map_err(db_err)?;
                if episodes_by_id.contains_key(&id) {
                    fts_scores.push((id, score));
                }
            }
        }
        if fts_scores.is_empty() && contains_thai(query) {
            for (id, (ep, _)) in &episodes_by_id {
                let overlap = thai_shingle_overlap(query, &ep.summary);
                if overlap > 0.0 {
                    fts_scores.push((id.clone(), overlap));
                }
            }
        }
    }
    let fts_ranks = dense_ranks_desc(&fts_scores, 1e-4);

    let mut heat_scores = Vec::with_capacity(episodes_by_id.len());
    let mut actr_scores = Vec::with_capacity(episodes_by_id.len());
    let mut metrics_by_id = HashMap::with_capacity(episodes_by_id.len());

    for (id, (ep, access_count)) in &episodes_by_id {
        let dt_hours = ((now_ms - ep.last_accessed_at).max(0) as f64) / 3_600_000.0;
        let stability_hours = 48.0 * (1.0 + f64::from(*access_count) * 0.5);
        let decayed_heat = f64::from(ep.memory_heat).max(0.0) * (-dt_hours / stability_hours).exp();
        let actr = (1.0 + f64::from(*access_count)).ln() - ACTR_DECAY_D * (1.0 + dt_hours).ln();
        heat_scores.push((id.clone(), decayed_heat));
        actr_scores.push((id.clone(), actr));
        metrics_by_id.insert(id.clone(), (decayed_heat, actr));
    }

    let heat_ranks = dense_ranks_desc(&heat_scores, 1e-6);
    let actr_ranks = dense_ranks_desc(&actr_scores, 1e-6);

    let mut scored: Vec<(String, f64, f64, f64, i64)> = Vec::with_capacity(episodes_by_id.len());
    for (id, (ep, _)) in &episodes_by_id {
        let mut rrf = 0.0_f64;
        if let Some(&r) = vec_ranks.get(id) {
            rrf += WEIGHT_VEC * (1.0 / (RRF_K + r as f64));
        }
        if let Some(&r) = fts_ranks.get(id) {
            rrf += WEIGHT_FTS * (1.0 / (RRF_K + r as f64));
        }
        if let Some(&r) = heat_ranks.get(id) {
            rrf += WEIGHT_HEAT * (1.0 / (RRF_K + r as f64));
        }
        if let Some(&r) = actr_ranks.get(id) {
            rrf += WEIGHT_ACTR * (1.0 / (RRF_K + r as f64));
        }
        let (decayed_heat, actr) = metrics_by_id.get(id).copied().unwrap_or((0.0, 0.0));
        scored.push((id.clone(), rrf, decayed_heat, actr, ep.last_accessed_at));
    }

    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| b.4.cmp(&a.4))
            .then_with(|| a.0.cmp(&b.0))
    });

    let selected_ids: Vec<String> = scored.into_iter().take(limit).map(|(id, ..)| id).collect();
    if selected_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut results = Vec::with_capacity(selected_ids.len());
    let tx = conn.transaction().map_err(db_err)?;
    for id in selected_ids {
        if let Some((mut ep, access_count)) = episodes_by_id.remove(&id) {
            let (decayed_heat, _) = metrics_by_id.get(&id).copied().unwrap_or((0.0, 0.0));
            let boosted_heat = compute_rehearsal_heat(ep.memory_heat, decayed_heat);
            let new_access = access_count.saturating_add(1);
            ep.last_accessed_at = now_ms;
            ep.memory_heat = boosted_heat;

            tx.execute(
                "UPDATE episodes
                 SET access_count = ?1,
                     last_accessed_at = ?2,
                     memory_heat = ?3
                 WHERE id = ?4",
                params![new_access, ep.last_accessed_at, ep.memory_heat, &ep.id],
            )
            .map_err(db_err)?;
            results.push(ep);
        }
    }
    tx.commit().map_err(db_err)?;

    Ok(results)
}

fn compute_rehearsal_heat(stored_heat: f32, decayed_heat: f64) -> f32 {
    let base = (stored_heat as f64).max(decayed_heat);
    (base * 0.8 + 0.2).clamp(0.0, 1.0) as f32
}

/// Assign 1-based tie-aware dense ranks (highest score receives rank 1).
fn dense_ranks_desc(items: &[(String, f64)], epsilon: f64) -> HashMap<String, usize> {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut ranks = HashMap::with_capacity(sorted.len());
    let mut current_rank = 0_usize;
    let mut prev_score: Option<f64> = None;

    for (idx, (id, score)) in sorted.into_iter().enumerate() {
        let is_tie = prev_score.is_some_and(|prev| (prev - score).abs() <= epsilon);
        if !is_tie {
            current_rank = idx + 1;
            prev_score = Some(score);
        }
        ranks.insert(id, current_rank);
    }
    ranks
}

fn build_fts5_query(query: &str) -> Option<String> {
    let tokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && !('\u{0E00}'..='\u{0E7F}').contains(&c))
        .filter(|tok| tok.chars().count() >= 2)
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect();

    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" OR "))
    }
}

fn contains_thai(text: &str) -> bool {
    text.chars().any(|c| ('\u{0E00}'..='\u{0E7F}').contains(&c))
}

fn thai_shingle_overlap(query: &str, haystack: &str) -> f64 {
    let q_chars: Vec<char> = query
        .chars()
        .filter(|c| ('\u{0E00}'..='\u{0E7F}').contains(c))
        .collect();
    if q_chars.len() < 2 {
        return 0.0;
    }
    let q_shingles: HashSet<String> = if q_chars.len() >= 3 {
        q_chars.windows(3).map(|w| w.iter().collect()).collect()
    } else {
        q_chars.windows(2).map(|w| w.iter().collect()).collect()
    };
    if q_shingles.is_empty() {
        return 0.0;
    }
    let hits = q_shingles
        .iter()
        .filter(|s| haystack.contains(s.as_str()))
        .count();
    (hits as f64) / (q_shingles.len() as f64)
}

/// Fetch recent dream diaries ordered newest first.
pub fn list_dream_diaries_query(conn: &Connection, limit: usize) -> SoulResult<Vec<DreamDiary>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT id, period_type, period_key, summary, emotional_tone, bond_delta, created_at
             FROM dream_diaries
             ORDER BY created_at DESC, rowid DESC
             LIMIT ?1",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            Ok(DreamDiary {
                id: row.get(0)?,
                period_type: row.get(1)?,
                period_key: row.get(2)?,
                summary: row.get(3)?,
                emotional_tone: row.get(4)?,
                bond_delta: row.get(5)?,
                created_at: row.get(6)?,
            })
        })
        .map_err(db_err)?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item.map_err(db_err)?);
    }
    Ok(out)
}

/// Load the persistent companion `RelationshipState`.
pub fn get_relationship_state_query(conn: &Connection) -> SoulResult<RelationshipState> {
    conn.query_row(
        "SELECT trust, warmth, familiarity, mood_baseline, total_turns, last_interaction_at
         FROM relationship_state
         WHERE id = 1",
        [],
        |row| {
            Ok(RelationshipState {
                trust: row.get(0)?,
                warmth: row.get(1)?,
                familiarity: row.get(2)?,
                mood_baseline: row.get(3)?,
                total_turns: row.get::<_, i64>(4)?.max(0) as u64,
                last_interaction_at: row.get(5)?,
            })
        },
    )
    .map_err(db_err)
}
