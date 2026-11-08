//! 媒体库查询：筛选条件 → SQL，列表与“全选筛选结果”共用同一套条件。

use crate::db::R;
use rusqlite::Connection;
use serde::Deserialize;
const WATERMARKED: &str = "EXISTS(SELECT 1 FROM watermark_marks wm WHERE wm.file_id=pf.id AND wm.size_bytes=pf.size_bytes AND wm.mtime_ms=pf.mtime_ms) OR EXISTS(SELECT 1 FROM processing_outputs po WHERE po.output_file_id=pf.id AND po.kind='watermark') OR EXISTS(SELECT 1 FROM watermark_hashes wh WHERE wh.content_hash=pf.sha256)";

#[derive(Deserialize, Default, Clone, Debug)]
pub struct Filter {
    pub q: Option<String>,
    pub media_type: Option<String>,
    pub source_id: Option<String>,
    pub year: Option<String>,
    pub month: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub confidence: Option<String>,
    pub unknown_date: Option<bool>,
    pub needs_review: Option<bool>,
    pub dup: Option<bool>,
    pub missing: Option<bool>,
    pub marked: Option<bool>,
    pub camera: Option<String>,
    pub star: Option<bool>,
    pub favorite: Option<bool>,
    pub live: Option<bool>,
    pub tag: Option<String>,
    pub job_id: Option<String>,
    pub watermarked: Option<bool>,
}

impl Filter {
    pub fn where_clause(&self) -> (String, Vec<String>) {
        let mut parts: Vec<String> = Vec::new();
        let mut args: Vec<String> = Vec::new();
        if let Some(marked)=self.watermarked {
            parts.push(format!("{}({WATERMARKED})",if marked {""} else {"NOT "}));
        }
        if let Some(id) = &self.job_id {
            args.push(id.clone());
            parts.push(format!("EXISTS(SELECT 1 FROM processing_outputs po JOIN media_files ofile ON ofile.id=po.output_file_id WHERE ofile.asset_id=a.id AND po.job_id=?{})",args.len()));
        }
        if let Some(q) = self.q.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            parts.push(
                "(pf.file_name LIKE ?x OR pf.absolute_path LIKE ?x OR a.camera LIKE ?x)"
                    .replace("?x", &format!("?{}", args.len() + 1)),
            );
            args.push(format!("%{}%", q.replace('%', "")));
        }
        if let Some(t) = &self.media_type {
            if !t.is_empty() && t != "all" {
                args.push(t.clone());
                parts.push(format!("a.media_type=?{}", args.len()));
            }
        }
        if let Some(s) = &self.source_id {
            if !s.is_empty() {
                args.push(s.clone());
                parts.push(format!(
                    "EXISTS(SELECT 1 FROM media_files fs WHERE fs.asset_id=a.id AND fs.source_id=?{})",
                    args.len()
                ));
            }
        }
        if let Some(y) = &self.year {
            if !y.is_empty() {
                args.push(y.clone());
                parts.push(format!("substr(a.effective_taken_at,1,4)=?{}", args.len()));
            }
        }
        if let Some(m) = &self.month {
            if !m.is_empty() {
                args.push(format!("{:0>2}", m));
                parts.push(format!("substr(a.effective_taken_at,6,2)=?{}", args.len()));
            }
        }
        if let Some(d) = &self.date_from {
            if !d.is_empty() {
                args.push(d.clone());
                parts.push(format!("a.effective_taken_at >= ?{}", args.len()));
            }
        }
        if let Some(d) = &self.date_to {
            if !d.is_empty() {
                args.push(d.clone());
                parts.push(format!("a.effective_taken_at <= ?{}", args.len()));
            }
        }
        if let Some(c) = &self.confidence {
            if !c.is_empty() && c != "all" {
                if c == "unknown" {
                    parts.push("a.effective_taken_at IS NULL".into());
                } else {
                    args.push(c.clone());
                    parts.push(format!("a.confidence=?{}", args.len()));
                }
            }
        }
        if self.unknown_date == Some(true) {
            parts.push("a.effective_taken_at IS NULL".into());
        }
        if self.needs_review == Some(true) {
            parts.push("a.needs_review=1".into());
        }
        if self.dup == Some(true) {
            parts.push(
                "EXISTS(SELECT 1 FROM media_files d1 WHERE d1.asset_id=a.id AND d1.sha256 IS NOT NULL
                 AND (SELECT count(DISTINCT lower(absolute_path)) FROM media_files d2 WHERE d2.sha256=d1.sha256)>1)"
                    .into(),
            );
        }
        if self.missing == Some(true) {
            parts.push("pf.availability='missing'".into());
        }
        if self.marked == Some(true) {
            parts.push("pf.marked=1".into());
        }
        if let Some(cam) = &self.camera {
            if !cam.is_empty() {
                args.push(format!("%{cam}%"));
                parts.push(format!("a.camera LIKE ?{}", args.len()));
            }
        }
        if self.star == Some(true) {
            parts.push("a.star=1".into());
        }
        if self.favorite == Some(true) {
            parts.push("a.favorite=1".into());
        }
        if self.live == Some(true) {
            parts.push("a.live_pair_id IS NOT NULL".into());
        }
        if let Some(tag) = self.tag.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            args.push(tag.to_string());
            parts.push(format!(
                "EXISTS(SELECT 1 FROM asset_tags at JOIN tags t ON t.tag_id=at.tag_id
                        WHERE at.asset_id=a.id AND t.name=?{} COLLATE NOCASE)",
                args.len()
            ));
        }
        let sql = if parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", parts.join(" AND "))
        };
        (sql, args)
    }
}

const FROM: &str = "FROM media_assets a
LEFT JOIN media_files pf ON pf.id = COALESCE(a.primary_file_id,
  (SELECT id FROM media_files p2 WHERE p2.asset_id=a.id ORDER BY p2.rowid LIMIT 1))";

pub fn order_clause(sort: &str, order: &str) -> String {
    let dir = if order.eq_ignore_ascii_case("asc") { "ASC" } else { "DESC" };
    match sort {
        "name" => format!(" ORDER BY pf.file_name {dir}"),
        "size" => format!(" ORDER BY pf.size_bytes {dir}"),
        "added" => format!(" ORDER BY a.created_at {dir}"),
        "trust" => format!(" ORDER BY a.needs_review ASC, a.effective_taken_at {dir}"),
        _ => format!(" ORDER BY a.effective_taken_at {dir} NULLS LAST, pf.file_name ASC"),
    }
}

pub fn list(
    conn: &Connection,
    filter: &Filter,
    sort: &str,
    order: &str,
    offset: i64,
    limit: i64,
) -> R<serde_json::Value> {
    let (w, args) = filter.where_clause();
    let count_sql = format!("SELECT count(*) {FROM}{w}");
    let total: i64 = conn
        .query_row(
            &count_sql,
            rusqlite::params_from_iter(args.iter()),
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    let sql = format!(
        "SELECT a.id, a.media_type, a.effective_taken_at, a.date_source, a.date_precision, a.confidence,
                a.needs_review, a.review_reason, a.width, a.height, a.duration_ms, a.camera,
                pf.id, pf.file_name, pf.absolute_path, pf.size_bytes, pf.availability, pf.ext,
                pf.sha256, pf.marked,
                (SELECT count(*) FROM media_files fc WHERE fc.asset_id=a.id) AS file_count,
                EXISTS(SELECT 1 FROM media_files d1 WHERE d1.asset_id=a.id AND d1.sha256 IS NOT NULL
                       AND (SELECT count(DISTINCT lower(absolute_path)) FROM media_files d2 WHERE d2.sha256=d1.sha256)>1) AS is_dup,
                (SELECT error FROM (SELECT json_extract(metadata_json,'$.error') AS error
                       FROM media_files x WHERE x.asset_id=a.id ORDER BY x.rowid LIMIT 1)) AS decode_error,
                a.star, a.favorite, a.live_pair_id,
                (SELECT group_concat(t.name, '|') FROM asset_tags at JOIN tags t ON t.tag_id=at.tag_id
                 WHERE at.asset_id=a.id) AS tag_names,
                ({WATERMARKED}) AS watermarked
         {FROM}{w}{} LIMIT ?{} OFFSET ?{}",
        order_clause(sort, order),
        args.len() + 1,
        args.len() + 2
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut params: Vec<rusqlite::types::Value> = args
        .iter()
        .map(|s| rusqlite::types::Value::Text(s.clone()))
        .collect();
    params.push(rusqlite::types::Value::Integer(limit));
    params.push(rusqlite::types::Value::Integer(offset));
    let items: Vec<serde_json::Value> = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(serde_json::json!({
                "id": r.get::<_, String>(0)?,
                "media_type": r.get::<_, String>(1)?,
                "taken_at": r.get::<_, Option<String>>(2)?,
                "date_source": r.get::<_, Option<String>>(3)?,
                "date_precision": r.get::<_, Option<String>>(4)?,
                "confidence": r.get::<_, String>(5)?,
                "needs_review": r.get::<_, i64>(6)? != 0,
                "review_reason": r.get::<_, Option<String>>(7)?,
                "width": r.get::<_, Option<i64>>(8)?,
                "height": r.get::<_, Option<i64>>(9)?,
                "duration_ms": r.get::<_, Option<i64>>(10)?,
                "camera": r.get::<_, Option<String>>(11)?,
                "file_id": r.get::<_, Option<String>>(12)?,
                "file_name": r.get::<_, Option<String>>(13)?,
                "path": r.get::<_, Option<String>>(14)?,
                "size_bytes": r.get::<_, Option<i64>>(15)?,
                "availability": r.get::<_, Option<String>>(16)?,
                "ext": r.get::<_, Option<String>>(17)?,
                "sha256": r.get::<_, Option<String>>(18)?,
                "marked": r.get::<_, Option<i64>>(19)? .unwrap_or(0) != 0,
                "file_count": r.get::<_, i64>(20)?,
                "dup": r.get::<_, i64>(21)? != 0,
                "decode_error": r.get::<_, Option<String>>(22)?,
                "star": r.get::<_, Option<i64>>(23)?.unwrap_or(0) != 0,
                "favorite": r.get::<_, Option<i64>>(24)?.unwrap_or(0) != 0,
                "live": r.get::<_, Option<String>>(25)?.is_some(),
                "watermarked": r.get::<_, bool>(27)?,
                "tags": r.get::<_, Option<String>>(26)?
                    .map(|s| s.split('|').filter(|t| !t.is_empty()).map(|t| t.to_string()).collect::<Vec<_>>())
                    .unwrap_or_default(),
            }))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(serde_json::json!({
        "total": total,
        "offset": offset,
        "limit": limit,
        "items": items,
    }))
}

pub fn ids(conn: &Connection, filter: &Filter) -> R<Vec<String>> {
    let (w, args) = filter.where_clause();
    let sql = format!("SELECT a.id {FROM}{w} ORDER BY a.effective_taken_at DESC NULLS LAST");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args.iter()), |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn facets(conn: &Connection) -> R<serde_json::Value> {
    let mut out = serde_json::json!({});
    let mut stmt = conn
        .prepare(
            "SELECT substr(effective_taken_at,1,4) y, count(*) c FROM media_assets
             WHERE effective_taken_at IS NOT NULL GROUP BY y ORDER BY y DESC",
        )
        .map_err(|e| e.to_string())?;
    let years: Vec<serde_json::Value> = stmt
        .query_map([], |r| {
            Ok(serde_json::json!({"year": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?}))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    out["years"] = serde_json::json!(years);

    let mut stmt = conn
        .prepare("SELECT media_type, count(*) FROM media_assets GROUP BY media_type")
        .map_err(|e| e.to_string())?;
    let types: Vec<serde_json::Value> = stmt
        .query_map([], |r| {
            Ok(serde_json::json!({"type": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?}))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    out["types"] = serde_json::json!(types);

    let mut stmt = conn
        .prepare("SELECT camera, count(*) FROM media_assets WHERE camera IS NOT NULL GROUP BY camera ORDER BY count(*) DESC LIMIT 20")
        .map_err(|e| e.to_string())?;
    let cams: Vec<serde_json::Value> = stmt
        .query_map([], |r| {
            Ok(serde_json::json!({"camera": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?}))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    out["cameras"] = serde_json::json!(cams);

    let mut stmt = conn
        .prepare(
            "SELECT t.name, count(*) FROM asset_tags at JOIN tags t ON t.tag_id=at.tag_id
             GROUP BY t.tag_id ORDER BY count(*) DESC, t.name COLLATE NOCASE",
        )
        .map_err(|e| e.to_string())?;
    let tags: Vec<serde_json::Value> = stmt
        .query_map([], |r| {
            Ok(serde_json::json!({"name": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?}))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    out["tags"] = serde_json::json!(tags);

    let count_where = |cond: &str| -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM media_assets WHERE {cond}"), [], |r| r.get(0))
            .unwrap_or(0)
    };
    out["starred"] = serde_json::json!(count_where("star=1"));
    out["favorites"] = serde_json::json!(count_where("favorite=1"));
    out["live"] = serde_json::json!(count_where("live_pair_id IS NOT NULL AND media_type='image'"));
    Ok(out)
}
