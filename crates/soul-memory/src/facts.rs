use rusqlite::{Connection, OptionalExtension, params};
use shared_types::{ChatRole, ChatTurn, MemoryFact, SoulError, SoulResult, new_id, now_timestamp_ms};

use crate::embedder::{EMBEDDING_DIM, Embedder, embedding_to_le_bytes, normalize_embedding};
use crate::schema::db_err;

/// Normalize a category or key string for case-insensitive, whitespace-collapsed contradiction matching.
pub fn normalize_fact_field(input: &str) -> String {
    input
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Normalize a value string for equivalence checks while preserving stored casing in `MemoryFact::value`.
pub(crate) fn normalize_fact_value(input: &str) -> String {
    input
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve an explicit 384-dim embedding or compute one locally via `Embedder`.
pub fn resolve_embedding(
    explicit: Option<&[f32]>,
    fallback_text: &str,
    embedder: &Embedder,
) -> SoulResult<Vec<f32>> {
    if let Some(raw) = explicit {
        if raw.len() != EMBEDDING_DIM {
            return Err(SoulError::InvalidInput(format!(
                "Expected {EMBEDDING_DIM}-dim embedding, got {}",
                raw.len()
            )));
        }
        let mut owned = raw.to_vec();
        if !normalize_embedding(&mut owned) {
            return Err(SoulError::InvalidInput(
                "Provided embedding must be finite and non-zero".into(),
            ));
        }
        return Ok(owned);
    }

    embedder.embed(fallback_text)
}

/// Insert or update a `MemoryFact`, superseding any conflicting active facts with matching `(category, key)`.
pub fn upsert_fact_tx(
    conn: &mut Connection,
    mut fact: MemoryFact,
    embedding: Option<&[f32]>,
    embedder: &Embedder,
) -> SoulResult<()> {
    if fact.id.trim().is_empty() {
        fact.id = new_id();
    }
    if fact.created_at <= 0 {
        fact.created_at = now_timestamp_ms();
    }
    if fact.last_accessed_at <= 0 {
        fact.last_accessed_at = fact.created_at;
    }

    let norm_cat = normalize_fact_field(&fact.category);
    let norm_key = normalize_fact_field(&fact.key);
    let norm_val = normalize_fact_value(&fact.value);
    if norm_cat.is_empty() || norm_key.is_empty() || norm_val.is_empty() {
        return Err(SoulError::InvalidInput(
            "Fact category, key, and value must be non-empty".into(),
        ));
    }

    let embed_text = format!("{}: {} {}", fact.category, fact.key, fact.value);
    let resolved_vec = resolve_embedding(embedding, &embed_text, embedder)?;
    let vec_bytes = embedding_to_le_bytes(&resolved_vec);

    let tx = conn.transaction().map_err(db_err)?;

    // Inspect active facts sharing normalized (category, key).
    let active_matches: Vec<(String, String, f32, f32, f32, u32)> = {
        let mut stmt = tx
            .prepare(
                "SELECT id, value, confidence, importance, memory_heat, access_count
                 FROM facts
                 WHERE norm_category = ?1 AND norm_key = ?2 AND superseded_by IS NULL",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![&norm_cat, &norm_key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f32>(2)?,
                    row.get::<_, f32>(3)?,
                    row.get::<_, f32>(4)?,
                    row.get::<_, u32>(5)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item.map_err(db_err)?);
        }
        out
    };

    let now_ms = fact.created_at.max(fact.last_accessed_at);
    let mut reinforced_existing_id: Option<String> = None;
    let mut conflicting_ids: Vec<String> = Vec::new();

    for (existing_id, existing_val, conf, imp, heat, access_cnt) in active_matches {
        if existing_id == fact.id {
            reinforced_existing_id = Some(existing_id);
        } else if normalize_fact_value(&existing_val) == norm_val {
            // Identical active fact already exists: reinforce it instead of creating a duplicate.
            let merged_conf = conf.max(fact.confidence).clamp(0.0, 1.0);
            let merged_imp = imp.max(fact.importance).clamp(0.0, 1.0);
            let merged_heat = (heat.max(fact.memory_heat) + 0.05).clamp(0.0, 1.0);
            let merged_access = access_cnt.saturating_add(1);
            tx.execute(
                "UPDATE facts
                 SET value = ?1,
                     confidence = ?2,
                     importance = ?3,
                     memory_heat = ?4,
                     access_count = ?5,
                     last_accessed_at = MAX(last_accessed_at, ?6)
                 WHERE id = ?7",
                params![
                    &fact.value,
                    merged_conf,
                    merged_imp,
                    merged_heat,
                    merged_access,
                    now_ms,
                    &existing_id,
                ],
            )
            .map_err(db_err)?;
            sync_fact_indexes(&tx, &existing_id, &fact.category, &fact.key, &fact.value, &vec_bytes)?;
            tx.commit().map_err(db_err)?;
            return Ok(());
        } else {
            conflicting_ids.push(existing_id);
        }
    }

    if let Some(existing_id) = reinforced_existing_id {
        tx.execute(
            "UPDATE facts
             SET category = ?1,
                 key = ?2,
                 norm_category = ?3,
                 norm_key = ?4,
                 value = ?5,
                 confidence = ?6,
                 importance = ?7,
                 memory_heat = ?8,
                 access_count = ?9,
                 last_accessed_at = ?10,
                 superseded_by = ?11,
                 superseded_at = ?12
             WHERE id = ?13",
            params![
                &fact.category,
                &fact.key,
                &norm_cat,
                &norm_key,
                &fact.value,
                fact.confidence.clamp(0.0, 1.0),
                fact.importance.clamp(0.0, 1.0),
                fact.memory_heat.clamp(0.0, 1.0),
                fact.access_count,
                fact.last_accessed_at,
                &fact.superseded_by,
                &fact.superseded_at,
                &existing_id,
            ],
        )
        .map_err(db_err)?;
    } else {
        tx.execute(
            "INSERT INTO facts (
                id, category, key, norm_category, norm_key, value,
                confidence, importance, memory_heat, access_count,
                created_at, last_accessed_at, superseded_by, superseded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, NULL)",
            params![
                &fact.id,
                &fact.category,
                &fact.key,
                &norm_cat,
                &norm_key,
                &fact.value,
                fact.confidence.clamp(0.0, 1.0),
                fact.importance.clamp(0.0, 1.0),
                fact.memory_heat.clamp(0.0, 1.0),
                fact.access_count,
                fact.created_at,
                fact.last_accessed_at,
            ],
        )
        .map_err(db_err)?;
    }

    // Supersede prior conflicting active facts while preserving full history in `facts`.
    for old_id in conflicting_ids {
        tx.execute(
            "UPDATE facts
             SET superseded_by = ?1,
                 superseded_at = ?2
             WHERE id = ?3",
            params![&fact.id, now_ms, &old_id],
        )
        .map_err(db_err)?;

        // Remove superseded fact vector from vec_facts so KNN search stays strictly active.
        tx.execute("DELETE FROM vec_facts WHERE fact_id = ?1", params![&old_id])
            .map_err(db_err)?;
    }

    sync_fact_indexes(
        &tx,
        &fact.id,
        &fact.category,
        &fact.key,
        &fact.value,
        &vec_bytes,
    )?;

    tx.commit().map_err(db_err)?;
    Ok(())
}

fn sync_fact_indexes(
    tx: &rusqlite::Transaction<'_>,
    fact_id: &str,
    category: &str,
    key: &str,
    value: &str,
    vec_bytes: &[u8],
) -> SoulResult<()> {
    tx.execute("DELETE FROM facts_fts WHERE fact_id = ?1", params![fact_id])
        .map_err(db_err)?;
    tx.execute(
        "INSERT INTO facts_fts (fact_id, category, key, value) VALUES (?1, ?2, ?3, ?4)",
        params![fact_id, category, key, value],
    )
    .map_err(db_err)?;

    tx.execute("DELETE FROM vec_facts WHERE fact_id = ?1", params![fact_id])
        .map_err(db_err)?;
    tx.execute(
        "INSERT INTO vec_facts (fact_id, embedding) VALUES (?1, ?2)",
        params![fact_id, vec_bytes],
    )
    .map_err(db_err)?;

    Ok(())
}

/// Query facts ordered by recency/creation, optionally including superseded facts.
pub fn list_facts_query(
    conn: &Connection,
    limit: usize,
    include_superseded: bool,
) -> SoulResult<Vec<MemoryFact>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let sql = if include_superseded {
        "SELECT id, category, key, value, confidence, importance, memory_heat,
                access_count, created_at, last_accessed_at, superseded_by, superseded_at
         FROM facts
         ORDER BY last_accessed_at DESC, created_at DESC, rowid DESC
         LIMIT ?1"
    } else {
        "SELECT id, category, key, value, confidence, importance, memory_heat,
                access_count, created_at, last_accessed_at, superseded_by, superseded_at
         FROM facts
         WHERE superseded_by IS NULL
         ORDER BY last_accessed_at DESC, created_at DESC, rowid DESC
         LIMIT ?1"
    };

    let mut stmt = conn.prepare(sql).map_err(db_err)?;
    let rows = stmt
        .query_map(params![limit as i64], row_to_fact)
        .map_err(db_err)?;

    let mut out = Vec::new();
    for item in rows {
        out.push(item.map_err(db_err)?);
    }
    Ok(out)
}

/// Look up a single fact by ID (helper for diagnostics/tests).
#[allow(dead_code)]
pub fn get_fact_by_id(conn: &Connection, id: &str) -> SoulResult<Option<MemoryFact>> {
    conn.query_row(
        "SELECT id, category, key, value, confidence, importance, memory_heat,
                access_count, created_at, last_accessed_at, superseded_by, superseded_at
         FROM facts WHERE id = ?1",
        params![id],
        row_to_fact,
    )
    .optional()
    .map_err(db_err)
}

pub fn row_to_fact(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryFact> {
    Ok(MemoryFact {
        id: row.get(0)?,
        category: row.get(1)?,
        key: row.get(2)?,
        value: row.get(3)?,
        confidence: row.get(4)?,
        importance: row.get(5)?,
        memory_heat: row.get(6)?,
        access_count: row.get(7)?,
        created_at: row.get(8)?,
        last_accessed_at: row.get(9)?,
        superseded_by: row.get(10)?,
        superseded_at: row.get(11)?,
    })
}

/// Extract structured personal facts (name, city/location, job, likes/dislikes) from a bilingual Thai/English turn.
pub fn extract_facts_from_turn(turn: &ChatTurn) -> Vec<MemoryFact> {
    if turn.role != ChatRole::User {
        return Vec::new();
    }
    extract_facts_from_text(&turn.content, turn.timestamp_ms)
}

/// Extract structured personal facts from raw bilingual Thai/English user text.
pub fn extract_facts_from_text(text: &str, timestamp_ms: i64) -> Vec<MemoryFact> {
    let ts = if timestamp_ms > 0 {
        timestamp_ms
    } else {
        now_timestamp_ms()
    };

    let mut facts = Vec::new();
    let clauses = split_clauses(text);

    for clause in clauses {
        let lower = clause.to_lowercase();

        // 1. Identity / Name
        if let Some(val) = extract_after_any(
            &clause,
            &lower,
            &[
                "my name is ",
                "call me ",
                "i am called ",
                "i'm called ",
                "ชื่อของฉันคือ",
                "ชื่อเล่นคือ",
                "เรียกฉันว่า",
                "เรียกเราว่า",
                "ฉันชื่อ",
                "ผมชื่อ",
                "หนูชื่อ",
                "เราชื่อ",
            ],
        ) {
            let cleaned = clean_extracted_value(&val, true);
            if !cleaned.is_empty() {
                facts.push(make_fact("identity", "name", cleaned, 0.92, 0.95, ts));
            }
        }

        // 2. Location / City
        if let Some(val) = extract_after_any(
            &clause,
            &lower,
            &[
                "i moved to ",
                "i am living in ",
                "i'm living in ",
                "i live in ",
                "i am from ",
                "i'm from ",
                "based in ",
                "อาศัยอยู่ที่",
                "ย้ายมาอยู่ที่",
                "ย้ายไปอยู่ที่",
                "ฉันอยู่ที่",
                "ผมอยู่ที่",
                "เราอยู่ที่",
                "อยู่จังหวัด",
                "อยู่ที่",
                "มาจาก",
            ],
        ) {
            let cleaned = clean_extracted_value(&val, false);
            if !cleaned.is_empty() {
                facts.push(make_fact("location", "city", cleaned, 0.88, 0.85, ts));
            }
        }

        // 3. Work / Occupation & Workplace
        if let Some(val) = extract_after_any(
            &clause,
            &lower,
            &[
                "i work as an ",
                "i work as a ",
                "i work as ",
                "my job is ",
                "ฉันทำงานเป็น",
                "ผมทำงานเป็น",
                "เราทำงานเป็น",
                "อาชีพของฉันคือ",
                "ทำงานเป็น",
                "อาชีพ",
            ],
        ) {
            let cleaned = clean_extracted_value(&val, false);
            if !cleaned.is_empty() {
                facts.push(make_fact("work", "occupation", cleaned, 0.88, 0.85, ts));
            }
        }

        if let Some(val) = extract_after_any(&clause, &lower, &["i work at ", "ทำงานที่"]) {
            let cleaned = clean_extracted_value(&val, false);
            if !cleaned.is_empty() {
                facts.push(make_fact("work", "workplace", cleaned, 0.85, 0.80, ts));
            }
        }

        // 4. Dislikes
        if let Some(val) = extract_after_any(
            &clause,
            &lower,
            &[
                "i don't like ",
                "i do not like ",
                "i really dislike ",
                "i dislike ",
                "i hate ",
                "ฉันไม่ชอบ",
                "ผมไม่ชอบ",
                "เราไม่ชอบ",
                "ไม่ชอบ",
                "เกลียด",
            ],
        ) {
            let cleaned = clean_extracted_value(&val, false);
            if !cleaned.is_empty() {
                facts.push(make_fact("preference", "dislikes", cleaned, 0.86, 0.78, ts));
            }
        }

        // 5. Likes (exclude negated matches like "ไม่ชอบ" or "don't like")
        if let Some(val) = extract_after_any_non_negated(
            &clause,
            &lower,
            &[
                "i really like ",
                "i love ",
                "i like ",
                "ฉันชอบ",
                "ผมชอบ",
                "เราชอบ",
                "ชอบกิน",
                "ชอบดื่ม",
                "ชอบ",
            ],
        ) {
            let cleaned = clean_extracted_value(&val, false);
            if !cleaned.is_empty() {
                facts.push(make_fact("preference", "likes", cleaned, 0.86, 0.78, ts));
            }
        }
    }

    facts
}

fn make_fact(
    category: &str,
    key: &str,
    value: String,
    confidence: f32,
    importance: f32,
    ts: i64,
) -> MemoryFact {
    MemoryFact {
        id: new_id(),
        category: category.to_string(),
        key: key.to_string(),
        value,
        confidence,
        importance,
        memory_heat: 0.75,
        access_count: 0,
        created_at: ts,
        last_accessed_at: ts,
        superseded_by: None,
        superseded_at: None,
    }
}

fn split_clauses(text: &str) -> Vec<String> {
    let mut raw_parts = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if matches!(ch, '.' | '!' | '?' | ';' | '\n' | '\r' | ',') {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                raw_parts.push(trimmed.to_string());
            }
            current.clear();
        } else {
            current.push(ch);
        }
    }
    let tail = current.trim();
    if !tail.is_empty() {
        raw_parts.push(tail.to_string());
    }

    // Also split on common English conjunctions (" and ", " but ") and Thai conjunctions.
    let mut final_clauses = Vec::new();
    for part in raw_parts {
        let mut sub_start = 0;
        let lower = part.to_lowercase();
        let separators = [" and ", " but ", " และ ", " แต่ "];
        let mut idx = 0;
        while idx < part.len() {
            if !part.is_char_boundary(idx) || !lower.is_char_boundary(idx) {
                idx += 1;
                continue;
            }
            let matched = separators
                .iter()
                .find(|sep| lower[idx..].starts_with(**sep));
            if let Some(sep) = matched {
                let seg = part[sub_start..idx].trim();
                if !seg.is_empty() {
                    final_clauses.push(seg.to_string());
                }
                idx += sep.len();
                sub_start = idx;
            } else {
                idx += part[idx..].chars().next().map_or(1, |c| c.len_utf8());
            }
        }
        let rem = part[sub_start..].trim();
        if !rem.is_empty() {
            final_clauses.push(rem.to_string());
        }
    }

    final_clauses
}

fn extract_after_any(original: &str, lower: &str, patterns: &[&str]) -> Option<String> {
    extract_after_any_inner(original, lower, patterns, false)
}

fn extract_after_any_non_negated(
    original: &str,
    lower: &str,
    patterns: &[&str],
) -> Option<String> {
    extract_after_any_inner(original, lower, patterns, true)
}

fn extract_after_any_inner(
    original: &str,
    lower: &str,
    patterns: &[&str],
    reject_negated: bool,
) -> Option<String> {
    // Only match if ASCII/English patterns align at a word boundary, or Thai pattern appears cleanly.
    for pat in patterns {
        let pat_lower = pat.to_lowercase();
        let mut search_from = 0;
        while search_from < lower.len() {
            let Some(rel_pos) = lower[search_from..].find(&pat_lower) else {
                break;
            };
            let byte_pos = search_from + rel_pos;
            let step = pat_lower.len().max(1);
            if byte_pos > 0 && pat_lower.is_ascii() {
                let prev = lower[..byte_pos].chars().last();
                if prev.is_some_and(|c| c.is_alphanumeric()) {
                    search_from = byte_pos + step;
                    continue;
                }
            }
            if reject_negated {
                let before = &lower[..byte_pos];
                if before.ends_with("ไม่")
                    || before.ends_with("don't ")
                    || before.ends_with("do not ")
                    || before.ends_with("not ")
                {
                    search_from = byte_pos + step;
                    continue;
                }
            }
            // Locate corresponding character offset in original string safely.
            let char_end = lower[..byte_pos].chars().count() + pat_lower.chars().count();
            let rest: String = original.chars().skip(char_end).collect();
            return Some(rest);
        }
    }
    None
}

fn clean_extracted_value(raw: &str, single_name_preferred: bool) -> String {
    let mut trimmed = raw.trim().to_string();

    // Trim leading Thai filler tokens or spaces ("ว่า ", "คือ ", ":").
    for prefix in [":", "ว่า", "คือ", "เป็น"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            trimmed = rest.trim().to_string();
        }
    }

    // Stop at common Thai polite particles / clause continuation markers.
    let stop_markers = [
        "ครับ",
        "ค่ะ",
        "นะคะ",
        "นะครับ",
        " นะ",
        "จ้า",
        "และ",
        "แต่",
        "เพราะ",
        "ตอนนี้",
        "แล้วก็",
        " ฉัน",
        " ผม",
        " เรา",
        " หนู",
        " อยู่ที่",
        " อยู่จังหวัด",
        " ทำงาน",
        " อาชีพ",
        " ไม่ชอบ",
        " ชอบ",
        " เกลียด",
        " working ",
        " because ",
    ];
    let mut best_char_end = trimmed.chars().count();
    let lower = trimmed.to_lowercase();
    for marker in stop_markers {
        if let Some(byte_idx) = lower.find(marker) {
            let char_idx = lower[..byte_idx].chars().count();
            if char_idx > 0 && char_idx < best_char_end {
                best_char_end = char_idx;
            }
        }
    }
    let mut result: String = trimmed.chars().take(best_char_end).collect();
    result = result
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '.' | ',' | '!' | '?'))
        .to_string();

    if single_name_preferred {
        // Keep up to 3 whitespace-separated tokens for names (e.g., "Alice" or "Alice Kim").
        let tokens: Vec<&str> = result.split_whitespace().take(3).collect();
        if !tokens.is_empty() {
            result = tokens.join(" ");
        }
    }

    // Cap extracted value at 64 chars cleanly on char boundary.
    result.chars().take(64).collect()
}
