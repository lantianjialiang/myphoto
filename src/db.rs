use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const SCHEMA_VERSION: i64 = 10;

pub type R<T> = Result<T, String>;

pub fn map_err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// 是否是锁冲突类错误（可重试）。SQLITE_BUSY_SNAPSHOT 也在其中：
/// 一个只读快照想升级成写入时，若期间别的连接提交过，SQLite 不等待、直接报 busy。
pub fn is_busy_err(msg: &str) -> bool {
    msg.contains("database is locked")
        || msg.contains("DatabaseBusy")
        || msg.contains("DatabaseLocked")
        || msg.contains("DatabaseSnapshot")
}

/// 开启写事务。一律用 `BEGIN IMMEDIATE`：
/// 延迟事务会先读后写，一旦期间有其他连接提交，后续写入直接报 SQLITE_BUSY_SNAPSHOT
/// 且 busy_timeout 不生效（整批写入全部失败）；IMMEDIATE 先拿写锁，排队等待而不是失败。
pub fn begin_write(conn: &Connection) -> R<()> {
    let mut last = String::new();
    for attempt in 0..5u32 {
        match conn.execute_batch("BEGIN IMMEDIATE") {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = map_err(e);
                if !is_busy_err(&last) {
                    return Err(last);
                }
                std::thread::sleep(std::time::Duration::from_millis(150 * u64::from(attempt + 1)));
            }
        }
    }
    Err(format!("等待写锁超时：{last}"))
}

/// 每库一条进程级常驻连接，永不关闭、不持任何事务。
///
/// 原因：WAL 库上「最后一条连接关闭」会删除 `-wal`/`-shm`，与另一线程正在打开新连接形成竞态，
/// 新连接会退化成只读，随后所有写入返回 `attempt to write a readonly database`
/// （扫描、任务线程与 API 线程并发时最容易触发）。保留一条常驻连接后该竞态不再出现。
/// 常驻连接不读不写，不会阻塞其他连接。
fn anchors() -> &'static Mutex<HashMap<PathBuf, Connection>> {
    static ANCHORS: OnceLock<Mutex<HashMap<PathBuf, Connection>>> = OnceLock::new();
    ANCHORS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn connect(path: &Path) -> R<Connection> {
    let conn = Connection::open(path).map_err(map_err)?;
    conn.execute_batch(
        "PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
         PRAGMA busy_timeout=8000; PRAGMA temp_store=MEMORY;",
    )
    .map_err(map_err)?;
    // 日志模式是写在库头里的持久设置，只在建库/迁移时切换一次。
    // 每次打开都执行 PRAGMA journal_mode=WAL 会做多余的 WAL 重置并额外加锁，
    // 在扫描线程持写锁时容易退化成 "database is locked"。
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap_or_default();
    if !mode.eq_ignore_ascii_case("wal") {
        let _ = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0));
    }
    Ok(conn)
}

fn ensure_anchor(path: &Path) {
    let Ok(mut map) = anchors().lock() else { return };
    if map.contains_key(path) {
        return;
    }
    // 常驻连接只为占位，不承担读写；建库/迁移仍由调用方那条连接完成。
    match connect(path) {
        Ok(conn) => {
            // 必须真正碰一次库（读元数据），SQLite 才会把它计入 WAL 共享内存，
            // 否则这条空转的连接挡不住「最后一条连接关闭时删除 -wal/-shm」。
            let _ = conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0));
            map.insert(path.to_path_buf(), conn);
        }
        Err(_) => {}
    }
}

pub fn open(path: &Path) -> R<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(map_err)?;
    }
    let conn = connect(path)?;
    migrate(&conn)?;
    // 建在迁移之后：此时库已非空、WAL 头已写入，常驻连接才能真正兜住 -wal/-shm。
    ensure_anchor(path);
    Ok(conn)
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap_or(0)
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let found = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map(|rows| rows.filter_map(|r| r.ok()).any(|c| c.eq_ignore_ascii_case(column)))
        .unwrap_or(false);
    found
}

/// 幂等迁移：建表用 IF NOT EXISTS，列级变更新增前先查 `PRAGMA table_info`。
/// 老库（v3 及更早）升级时不会被重建，也不会丢数据。
/// 版本已是最新时直接返回：常态下打开数据库不再执行任何 DDL，避免与后台任务抢写锁。
fn migrate(conn: &Connection) -> R<()> {
    if user_version(conn) >= SCHEMA_VERSION {
        return Ok(());
    }
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS sources (
  id TEXT PRIMARY KEY,
  root_path TEXT NOT NULL UNIQUE,
  label TEXT,
  is_online INTEGER NOT NULL DEFAULT 1,
  last_scan_at TEXT,
  options_json TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS media_assets (
  id TEXT PRIMARY KEY,
  media_type TEXT NOT NULL,
  effective_taken_at TEXT,
  date_source TEXT,
  date_precision TEXT NOT NULL DEFAULT 'day',
  confidence TEXT NOT NULL DEFAULT 'none',
  override_taken_at TEXT,
  needs_review INTEGER NOT NULL DEFAULT 0,
  review_reason TEXT,
  primary_file_id TEXT,
  width INTEGER,
  height INTEGER,
  duration_ms INTEGER,
  camera TEXT,
  gps_lat REAL,
  gps_lon REAL,
  metadata_json TEXT,
  star INTEGER NOT NULL DEFAULT 0,
  favorite INTEGER NOT NULL DEFAULT 0,
  phash TEXT,
  live_pair_id TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS media_files (
  id TEXT PRIMARY KEY,
  asset_id TEXT NOT NULL REFERENCES media_assets(id) ON DELETE CASCADE,
  source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  absolute_path TEXT NOT NULL,
  rel_dir TEXT,
  file_name TEXT NOT NULL,
  ext TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  mtime_ms INTEGER NOT NULL,
  sha256 TEXT,
  availability TEXT NOT NULL DEFAULT 'online',
  last_seen_scan_id TEXT,
  metadata_json TEXT,
  taken_at TEXT,
  date_source TEXT,
  date_confidence TEXT,
  candidates_json TEXT,
  marked INTEGER NOT NULL DEFAULT 0,
  added_at TEXT NOT NULL,
  UNIQUE(source_id, absolute_path)
);
CREATE INDEX IF NOT EXISTS idx_files_asset ON media_files(asset_id);
CREATE INDEX IF NOT EXISTS idx_files_sha ON media_files(sha256);
CREATE INDEX IF NOT EXISTS idx_files_size ON media_files(size_bytes);
CREATE INDEX IF NOT EXISTS idx_files_taken ON media_files(taken_at);

CREATE TABLE IF NOT EXISTS media_previews (
  asset_id TEXT NOT NULL,
  preview_kind TEXT NOT NULL,
  cache_path TEXT NOT NULL,
  generator_version TEXT,
  updated_at TEXT,
  PRIMARY KEY (asset_id, preview_kind)
);

CREATE TABLE IF NOT EXISTS processing_outputs (
  output_file_id TEXT PRIMARY KEY REFERENCES media_files(id) ON DELETE CASCADE,
  source_asset_id TEXT REFERENCES media_assets(id) ON DELETE SET NULL,
  job_id TEXT NOT NULL,
  kind TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_processing_job ON processing_outputs(job_id);
CREATE TABLE IF NOT EXISTS watermark_marks (
 file_id TEXT PRIMARY KEY REFERENCES media_files(id) ON DELETE CASCADE,
 content_hash TEXT NOT NULL, size_bytes INTEGER NOT NULL, mtime_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_watermark_hash ON watermark_marks(content_hash);
CREATE TABLE IF NOT EXISTS watermark_hashes (content_hash TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS watermark_rotation (
 asset_id TEXT PRIMARY KEY REFERENCES media_assets(id) ON DELETE CASCADE,
 degrees INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS jobs (
  id TEXT PRIMARY KEY,
  job_type TEXT NOT NULL,
  status TEXT NOT NULL,
  total INTEGER NOT NULL DEFAULT 0,
  done INTEGER NOT NULL DEFAULT 0,
  succeeded INTEGER NOT NULL DEFAULT 0,
  failed INTEGER NOT NULL DEFAULT 0,
  skipped INTEGER NOT NULL DEFAULT 0,
  payload_json TEXT,
  message TEXT,
  error TEXT,
  created_at TEXT NOT NULL,
  started_at TEXT,
  finished_at TEXT
);

CREATE TABLE IF NOT EXISTS job_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  file_id TEXT,
  source_path TEXT,
  target_path TEXT,
  status TEXT NOT NULL,
  error_code TEXT,
  error TEXT,
  verified_hash TEXT,
  bytes INTEGER,
  updated_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_job_items_job ON job_items(job_id);

CREATE TABLE IF NOT EXISTS operation_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  operation TEXT NOT NULL,
  source_path TEXT,
  target_path TEXT,
  before_hash TEXT,
  after_hash TEXT,
  timestamp TEXT NOT NULL,
  status TEXT NOT NULL,
  job_id TEXT,
  detail TEXT
);

CREATE TABLE IF NOT EXISTS archive_plans (
  id TEXT PRIMARY KEY,
  out_dir TEXT NOT NULL,
  template TEXT NOT NULL,
  conflict TEXT NOT NULL,
  total INTEGER NOT NULL DEFAULT 0,
  total_bytes INTEGER NOT NULL DEFAULT 0,
  conflicts INTEGER NOT NULL DEFAULT 0,
  mode TEXT NOT NULL DEFAULT 'copy',
  undone_at TEXT,
  created_at TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'planned'
);

CREATE TABLE IF NOT EXISTS archive_plan_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  plan_id TEXT NOT NULL REFERENCES archive_plans(id) ON DELETE CASCADE,
  file_id TEXT NOT NULL,
  asset_id TEXT NOT NULL,
  source_path TEXT NOT NULL,
  target_path TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  verified_hash TEXT,
  note TEXT
);
CREATE INDEX IF NOT EXISTS idx_plan_items_plan ON archive_plan_items(plan_id);

CREATE TABLE IF NOT EXISTS tags (
  tag_id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  color TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS asset_tags (
  asset_id TEXT NOT NULL REFERENCES media_assets(id) ON DELETE CASCADE,
  tag_id TEXT NOT NULL REFERENCES tags(tag_id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  PRIMARY KEY (asset_id, tag_id)
);
CREATE INDEX IF NOT EXISTS idx_asset_tags_tag ON asset_tags(tag_id);

CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS watermark_presets (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  config_json TEXT NOT NULL,
  builtin INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);

-- 覆盖原图前留下的原件备份，用于「一键还原」。state: done（可还原）/ restored（已还原）/ missing（备份丢了）
CREATE TABLE IF NOT EXISTS watermark_backups (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id TEXT NOT NULL,
  asset_id TEXT NOT NULL,
  file_path TEXT NOT NULL,
  backup_path TEXT NOT NULL,
  bytes INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL DEFAULT 'done',
  created_at TEXT NOT NULL,
  restored_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_wmbackup_job ON watermark_backups(job_id, id);

-- 批量旋转/翻转覆盖原图前留下的备份，用于「一键还原」
CREATE TABLE IF NOT EXISTS edit_backups (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id TEXT NOT NULL,
  asset_id TEXT NOT NULL,
  file_path TEXT NOT NULL,
  backup_path TEXT NOT NULL,
  bytes INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL DEFAULT 'done',
  created_at TEXT NOT NULL,
  restored_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_editbackup_job ON edit_backups(job_id, id);
"#,
    )
    .map_err(map_err)?;

    // v4 列级迁移（老库补齐）
    let cols: [(&str, &str, &str); 4] = [
        ("media_assets", "star", "INTEGER NOT NULL DEFAULT 0"),
        ("media_assets", "favorite", "INTEGER NOT NULL DEFAULT 0"),
        ("media_assets", "phash", "TEXT"),
        ("media_assets", "live_pair_id", "TEXT"),
    ];
    for (table, col, decl) in cols {
        if !has_column(conn, table, col) {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl};"))
                .map_err(map_err)?;
        }
    }
    for (col, decl) in [
        ("mode", "TEXT NOT NULL DEFAULT 'copy'"),
        ("undone_at", "TEXT"),
    ] {
        if !has_column(conn, "archive_plans", col) {
            conn.execute_batch(&format!("ALTER TABLE archive_plans ADD COLUMN {col} {decl};"))
                .map_err(map_err)?;
        }
    }
    if !has_column(conn, "archive_plan_items", "verified_hash") {
        conn.execute_batch("ALTER TABLE archive_plan_items ADD COLUMN verified_hash TEXT;")
            .map_err(map_err)?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_assets_phash ON media_assets(phash);
         CREATE INDEX IF NOT EXISTS idx_assets_live ON media_assets(live_pair_id);",
    )
    .map_err(map_err)?;

    if user_version(conn) < SCHEMA_VERSION {
        // 内置水印预设：只在缺失时写入，用户改过的不会被覆盖。
        crate::watermark::seed_presets(conn)?;
        conn.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION};"))
            .map_err(map_err)?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> AppPaths {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir());
        let data_dir = base.join("PhotoVideoManager");
        AppPaths {
            db_path: data_dir.join("library.db"),
            cache_dir: data_dir.join("cache"),
            log_dir: data_dir.join("logs"),
            data_dir,
        }
    }
}

pub fn get_setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM settings WHERE key=?1",
        params![key],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> R<()> {
    conn.execute(
        "INSERT INTO settings(key,value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )
    .map_err(map_err)?;
    Ok(())
}

pub fn default_template() -> String {
    "{YYYY}/{YYYY-MM}/{original_name}".to_string()
}

pub fn log_op(
    conn: &Connection,
    operation: &str,
    source: Option<&str>,
    target: Option<&str>,
    before: Option<&str>,
    after: Option<&str>,
    status: &str,
    job_id: Option<&str>,
    detail: Option<&str>,
) {
    let _ = conn.execute(
        "INSERT INTO operation_log(operation,source_path,target_path,before_hash,after_hash,timestamp,status,job_id,detail)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            operation,
            source,
            target,
            before,
            after,
            crate::util::now_iso(),
            status,
            job_id,
            detail
        ],
    );
}
