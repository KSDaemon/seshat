use std::path::Path;

use crate::error::CliError;

pub fn run_debug(db_path: &Path, branch_id: &str) -> Result<(), CliError> {
    let conn = rusqlite::Connection::open(db_path).map_err(|e| CliError::CommandFailed {
        command: "debug".to_owned(),
        reason: format!("failed to open database: {e}"),
    })?;

    let sql = "
        SELECT id, description, nature, weight, confidence,
               adoption_count, total_count, ext_data
        FROM nodes
        WHERE nature IN ('convention', 'observation')
          AND branch_id = ?1
          AND (json_extract(ext_data, '$.user_rejected') IS NULL
               OR json_extract(ext_data, '$.user_rejected') != 1)
          AND (json_extract(ext_data, '$.source') IS NULL
               OR json_extract(ext_data, '$.source') != 'user')
        ORDER BY confidence DESC
    ";

    let mut stmt = conn.prepare(sql).map_err(|e| CliError::CommandFailed {
        command: "debug".to_owned(),
        reason: e.to_string(),
    })?;

    let rows = stmt
        .query_map(rusqlite::params![branch_id], |row| {
            let id: i64 = row.get(0)?;
            let description: String = row.get(1)?;
            let nature: String = row.get(2)?;
            let weight: String = row.get(3)?;
            let confidence: f64 = row.get(4)?;
            let adoption_count: u32 = row.get(5)?;
            let total_count: u32 = row.get(6)?;
            let ext_data: Option<String> = row.get(7)?;
            Ok((
                id,
                description,
                nature,
                weight,
                confidence,
                adoption_count,
                total_count,
                ext_data,
            ))
        })
        .map_err(|e| CliError::CommandFailed {
            command: "debug".to_owned(),
            reason: e.to_string(),
        })?;

    let mut idx = 0;
    for row_result in rows {
        let (_id, description, nature, weight, confidence, adoption_count, total_count, ext_data) =
            row_result.map_err(|e| CliError::CommandFailed {
                command: "debug".to_owned(),
                reason: e.to_string(),
            })?;

        idx += 1;
        let conf_pct = (confidence.clamp(0.0, 1.0) * 100.0).round() as u32;
        let adoption_pct = if total_count > 0 {
            ((adoption_count as f64 / total_count as f64) * 100.0).round() as u32
        } else {
            0
        };

        println!(
            "═══ {}/─ {} ═══ {}|{}|{}% | {}/{} ({}%)",
            idx, description, nature, weight, conf_pct, adoption_count, total_count, adoption_pct
        );

        let ext: Option<serde_json::Value> = ext_data
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());

        let evidence = match ext
            .as_ref()
            .and_then(|e| e.get("evidence"))
            .and_then(|v| v.as_array())
        {
            Some(arr) => arr,
            None => {
                println!("  [no evidence]");
                continue;
            }
        };

        for (ei, item) in evidence.iter().enumerate() {
            let file = item.get("file").and_then(|v| v.as_str()).unwrap_or("?");
            let line = item.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let end_line = item
                .get("end_line")
                .and_then(|v| v.as_u64())
                .unwrap_or(line as u64) as u32;
            let snippet = item
                .get("snippet")
                .and_then(|v| {
                    v.get("content")
                        .and_then(|c| c.as_str())
                        .or_else(|| v.as_str())
                })
                .unwrap_or("");
            let snippet_start_line = item
                .get("snippet_start_line")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;

            println!(
                "  [{ei}] {file}  line={line}..{end_line}  ssl={snippet_start_line}  snippet_len={}",
                snippet.len()
            );
            if !snippet.is_empty() {
                for (li, l) in snippet.lines().enumerate() {
                    let actual_line = if snippet_start_line > 0 {
                        snippet_start_line as usize + li
                    } else {
                        line as usize + li
                    };
                    let marker = if actual_line >= line as usize && actual_line <= end_line as usize
                    {
                        ">>>"
                    } else {
                        "   "
                    };
                    let numbered_line = actual_line + 1;
                    println!("    {marker} {numbered_line:>4} | {l}");
                }
            }
        }
    }

    Ok(())
}
