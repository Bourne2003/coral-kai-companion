use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, backup::Backup};
use shared_types::{CharacterId, SoulError, SoulResult, new_id};

use crate::schema::{
    REQUIRED_CORE_TABLES, REQUIRED_VIRTUAL_TABLES, db_err, ensure_schema_and_seeds,
};

const SQLITE_HEADER_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Atomically export the active SQLite memory database into a self-contained `.sqlite` file.
pub fn export_snapshot_sync(src_conn: &Connection, target_path: &Path) -> SoulResult<String> {
    if target_path.as_os_str().is_empty() {
        return Err(SoulError::InvalidInput(
            "Target snapshot path cannot be empty".into(),
        ));
    }
    if target_path.is_dir() {
        return Err(SoulError::InvalidInput(format!(
            "Target snapshot path is a directory: {}",
            target_path.display()
        )));
    }

    if let Some(parent) = target_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let parent_dir = target_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("soul_memory.sqlite");
    let tmp_path = parent_dir.join(format!(".{file_name}.tmp.{}", new_id()));

    let write_res = (|| -> SoulResult<()> {
        let mut dst_conn = Connection::open(&tmp_path).map_err(db_err)?;
        {
            let backup = Backup::new(src_conn, &mut dst_conn).map_err(db_err)?;
            backup
                .run_to_completion(128, Duration::from_millis(2), None)
                .map_err(db_err)?;
        }
        // Ensure exported snapshot is strictly single-file without sidecar WAL files.
        dst_conn
            .execute_batch("PRAGMA journal_mode = DELETE; PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(db_err)?;
        dst_conn.close().map_err(|(_, e)| db_err(e))?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }

    if let Err(err) = std::fs::rename(&tmp_path, target_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err.into());
    }

    Ok(target_path.to_string_lossy().into_owned())
}

/// Validate a `.sqlite` snapshot file and atomically restore it into the active database.
/// Returns the total number of imported memory records (`turns + facts + episodes + dream_diaries`).
pub fn import_snapshot_sync(
    dst_conn: &mut Connection,
    source_path: &Path,
    character_id: CharacterId,
) -> SoulResult<usize> {
    if !source_path.exists() || !source_path.is_file() {
        return Err(SoulError::InvalidInput(format!(
            "Snapshot file does not exist: {}",
            source_path.display()
        )));
    }

    validate_sqlite_magic_header(source_path)?;

    let src_conn = Connection::open_with_flags(
        source_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| SoulError::InvalidInput(format!("Unable to open SQLite snapshot: {e}")))?;

    // Verify full B-tree / page integrity before mutating the active database.
    let integrity_rows: Vec<String> = {
        let mut stmt = src_conn
            .prepare("PRAGMA integrity_check")
            .map_err(|e| SoulError::InvalidInput(format!("Snapshot integrity query failed: {e}")))?;
        let mapped = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| SoulError::InvalidInput(format!("Snapshot integrity check failed: {e}")))?;
        let mut out = Vec::new();
        for item in mapped {
            out.push(item.map_err(db_err)?);
        }
        out
    };
    if integrity_rows != vec!["ok".to_string()] {
        return Err(SoulError::InvalidInput(format!(
            "Snapshot failed PRAGMA integrity_check: {:?}",
            integrity_rows
        )));
    }

    // Verify all required Coral&Kai Companion tables exist.
    let existing_tables: HashSet<String> = {
        let mut stmt = src_conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(db_err)?;
        let mut set = HashSet::new();
        for item in rows {
            set.insert(item.map_err(db_err)?);
        }
        set
    };

    for &table in REQUIRED_CORE_TABLES
        .iter()
        .chain(REQUIRED_VIRTUAL_TABLES.iter())
    {
        if !existing_tables.contains(table) {
            return Err(SoulError::InvalidInput(format!(
                "Snapshot is missing required Coral&Kai Companion table `{table}`"
            )));
        }
    }

    let imported_count = count_snapshot_records(&src_conn)?;

    {
        let backup = Backup::new(&src_conn, dst_conn).map_err(db_err)?;
        backup
            .run_to_completion(128, Duration::from_millis(2), None)
            .map_err(db_err)?;
    }

    // Ensure foreign keys, extended inner-life tables, & seed rows remain consistent after restore.
    dst_conn
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
        .map_err(db_err)?;
    ensure_schema_and_seeds(dst_conn, character_id)?;

    Ok(imported_count)
}

fn validate_sqlite_magic_header(path: &Path) -> SoulResult<()> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    if meta.len() < 100 {
        return Err(SoulError::InvalidInput(format!(
            "File is too small to be a valid SQLite database: {}",
            path.display()
        )));
    }
    let mut header = [0_u8; 16];
    file.read_exact(&mut header)?;
    if &header != SQLITE_HEADER_MAGIC {
        return Err(SoulError::InvalidInput(format!(
            "Invalid SQLite header magic in {}",
            path.display()
        )));
    }
    Ok(())
}

fn count_snapshot_records(conn: &Connection) -> SoulResult<usize> {
    let mut total: usize = 0;
    for table in ["turns", "facts", "episodes", "dream_diaries"] {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let count: i64 = conn.query_row(&sql, [], |r| r.get(0)).map_err(db_err)?;
        total = total.saturating_add(count.max(0) as usize);
    }
    Ok(total)
}
