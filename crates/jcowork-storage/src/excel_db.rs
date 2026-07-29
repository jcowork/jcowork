//! Excel-to-SQLite import for uploaded spreadsheets.
//!
//! Every uploaded Excel file (.xlsx/.xls) is parsed into its own SQLite
//! database under `{data_dir}/{user_id}/excel_db/`. Each worksheet becomes
//! one table (first non-empty row is the header) and every column gets a
//! plain index so the agent can filter/sort efficiently.
//!
//! The database file mirrors the workspace-relative path of the source file:
//! `reports/销售数据.xlsx` -> `excel_db/reports/销售数据.db`. The relative
//! name without extension (e.g. `reports/销售数据`) is the handle used by the
//! `excel_db` agent tool.

use anyhow::{Context, Result, bail};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tracing::{info, warn};

/// Maximum data rows imported per sheet (safety valve for huge files).
const MAX_ROWS_PER_SHEET: usize = 100_000;
/// Maximum columns imported per sheet.
const MAX_COLUMNS_PER_SHEET: usize = 500;
/// Maximum header name length (characters).
const MAX_HEADER_LEN: usize = 64;

/// A single cell value, reduced to SQLite storage classes.
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

/// A parsed worksheet: name, header row and data rows.
#[derive(Debug, Clone)]
pub struct SheetData {
    pub name: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<CellValue>>,
    /// True when rows were dropped due to the per-sheet row cap.
    pub truncated: bool,
}

/// Import result for one table.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TableSummary {
    pub sheet_name: String,
    pub table_name: String,
    /// (column name, SQL type) pairs.
    pub columns: Vec<(String, String)>,
    pub row_count: usize,
    /// True when rows were dropped due to MAX_ROWS_PER_SHEET.
    pub truncated: bool,
}

/// Import result for a whole workbook.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportSummary {
    /// Relative database name used by the excel_db tool (no .db extension).
    pub db_name: String,
    /// Workspace-relative path of the source Excel file.
    pub source_path: String,
    pub tables: Vec<TableSummary>,
    /// Sheets that could not be imported (empty or unreadable).
    pub skipped_sheets: Vec<String>,
}

impl ImportSummary {
    /// Human-readable text stored in the workspace FTS index so the file is
    /// searchable and `doc_search` shows a useful preview.
    pub fn to_index_text(&self) -> String {
        let mut out = format!(
            "Excel 工作簿已解析为 SQLite 数据库。\n数据库名称: {}\n来源文件: {}\n数据表数量: {}\n",
            self.db_name,
            self.source_path,
            self.tables.len()
        );
        for t in &self.tables {
            let cols: Vec<String> = t
                .columns
                .iter()
                .take(20)
                .map(|(n, ty)| format!("{} {}", n, ty))
                .collect();
            let more = if t.columns.len() > 20 { ", ..." } else { "" };
            out.push_str(&format!(
                "\n表 \"{}\"（工作表 \"{}\"）: {} 列 [{}]{}，{} 行{}",
                t.table_name,
                t.sheet_name,
                t.columns.len(),
                cols.join(", "),
                more,
                t.row_count,
                if t.truncated { "（已截断）" } else { "" },
            ));
        }
        if !self.skipped_sheets.is_empty() {
            out.push_str(&format!("\n跳过的工作表: {}", self.skipped_sheets.join(", ")));
        }
        out
    }
}

/// Basic info about one imported Excel database, used by the tool's `list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExcelDbInfo {
    /// Relative database name (no .db extension) — the tool's `db` parameter.
    pub name: String,
    pub size_bytes: u64,
    pub source_file: Option<String>,
    pub imported_at: Option<String>,
    /// (table name, row count) pairs.
    pub tables: Vec<(String, i64)>,
}

/// Full schema of one table, used by the tool's `list` with a db argument.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TableInfo {
    pub name: String,
    /// (column name, SQL type) pairs.
    pub columns: Vec<(String, String)>,
    pub row_count: i64,
    pub indexes: Vec<String>,
}

/// Directory holding all imported Excel databases for a user.
pub fn excel_db_dir(data_dir: &str, user_id: &str) -> PathBuf {
    Path::new(data_dir).join(user_id).join("excel_db")
}

/// Database file path for a workspace-relative source Excel path.
pub fn db_path_for(data_dir: &str, user_id: &str, source_rel_path: &str) -> PathBuf {
    excel_db_dir(data_dir, user_id).join(format!("{}.db", db_name_for(source_rel_path)))
}

/// Relative database name (no extension) for a source path.
pub fn db_name_for(source_rel_path: &str) -> String {
    let p = Path::new(source_rel_path);
    let without_ext = p.with_extension("");
    without_ext.to_string_lossy().replace('\\', "/")
}

/// Resolve and validate a user-supplied database name to a file path.
/// Rejects path traversal; the database must already exist.
pub async fn resolve_db_path(data_dir: &str, user_id: &str, db_name: &str) -> Result<PathBuf> {
    let name = db_name.trim();
    let name = name.strip_suffix(".db").unwrap_or(name);
    if name.is_empty()
        || name.contains("..")
        || name.starts_with('/')
        || name.starts_with('\\')
        || name.contains('\0')
    {
        bail!("Invalid database name: {}", db_name);
    }
    let path = excel_db_dir(data_dir, user_id).join(format!("{}.db", name));
    if !path.exists() {
        bail!(
            "Excel database '{}' not found. Use action=\"list\" to see available databases.",
            name
        );
    }
    Ok(path)
}

/// Open an existing Excel database (read-write).
pub async fn open_db(db_path: &Path) -> Result<SqlitePool> {
    let url = format!("sqlite:{}?mode=rw", db_path.display());
    let options = SqliteConnectOptions::from_str(&url)?
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    Ok(pool)
}

/// Quote a SQLite identifier (table/column name) with double quotes.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

// --- Excel parsing (calamine, synchronous) ---

/// Parse an Excel workbook into sheet data. Returns (sheets, skipped sheet names).
/// This is CPU/IO-bound and synchronous — call inside `spawn_blocking`.
pub fn parse_excel_file(path: &Path) -> Result<(Vec<SheetData>, Vec<String>)> {
    use calamine::{Data, Reader};

    let mut workbook = calamine::open_workbook_auto(path)
        .with_context(|| format!("Cannot open Excel file: {}", path.display()))?;

    let sheet_names = workbook.sheet_names().to_vec();
    let mut sheets = Vec::new();
    let mut skipped = Vec::new();

    for name in sheet_names {
        let range = match workbook.worksheet_range(&name) {
            Ok(r) => r,
            Err(e) => {
                warn!(sheet = %name, err = %e, "Skipping unreadable sheet");
                skipped.push(name);
                continue;
            }
        };

        let cell_of = |d: &Data| -> CellValue {
            match d {
                Data::Empty => CellValue::Null,
                Data::Int(i) => CellValue::Int(*i),
                Data::Float(f) => CellValue::Real(*f),
                Data::String(s) => {
                    if s.trim().is_empty() {
                        CellValue::Null
                    } else {
                        CellValue::Text(s.clone())
                    }
                }
                Data::Bool(b) => CellValue::Int(if *b { 1 } else { 0 }),
                Data::DateTime(dt) => excel_serial_to_datetime(dt.as_f64())
                    .map(CellValue::Text)
                    .unwrap_or(CellValue::Real(dt.as_f64())),
                Data::DateTimeIso(s) | Data::DurationIso(s) => CellValue::Text(s.clone()),
                Data::Error(_) => CellValue::Null,
            }
        };

        let raw_rows: Vec<Vec<CellValue>> = range
            .rows()
            .map(|r| r.iter().map(cell_of).collect())
            .collect();

        // First non-empty row is the header; skip the sheet when it is empty.
        let Some(hidx) = raw_rows
            .iter()
            .position(|r| r.iter().any(|c| *c != CellValue::Null))
        else {
            skipped.push(name);
            continue;
        };
        let header_row = &raw_rows[hidx];
        let data_rows = &raw_rows[hidx + 1..];

        let ncols = data_rows
            .iter()
            .map(|r| r.len())
            .chain(std::iter::once(header_row.len()))
            .max()
            .unwrap_or(0)
            .min(MAX_COLUMNS_PER_SHEET);
        if ncols == 0 {
            skipped.push(name);
            continue;
        }

        let header_text = |c: &CellValue| -> String {
            match c {
                CellValue::Int(i) => i.to_string(),
                CellValue::Real(f) => {
                    if f.fract() == 0.0 {
                        format!("{:.0}", f)
                    } else {
                        f.to_string()
                    }
                }
                CellValue::Text(s) => s.trim().to_string(),
                CellValue::Null => String::new(),
            }
        };

        let mut headers: Vec<String> = (0..ncols)
            .map(|i| {
                let h = header_row.get(i).map(header_text).unwrap_or_default();
                let h: String = h.chars().take(MAX_HEADER_LEN).collect();
                if h.is_empty() {
                    format!("column_{}", i + 1)
                } else {
                    h
                }
            })
            .collect();
        dedup_headers(&mut headers);

        let mut truncated = false;
        let mut rows: Vec<Vec<CellValue>> = Vec::new();
        for r in data_rows {
            if rows.len() >= MAX_ROWS_PER_SHEET {
                truncated = true;
                break;
            }
            let mut row: Vec<CellValue> = (0..ncols)
                .map(|i| r.get(i).cloned().unwrap_or(CellValue::Null))
                .collect();
            // Skip fully-empty rows (e.g. formatting-only tail rows)
            if row.iter().all(|c| *c == CellValue::Null) {
                continue;
            }
            row.truncate(ncols);
            rows.push(row);
        }

        sheets.push(SheetData {
            name,
            headers,
            rows,
            truncated,
        });
    }

    Ok((sheets, skipped))
}

/// Convert an Excel date serial number (1900 date system) to "YYYY-MM-DD HH:MM:SS".
fn excel_serial_to_datetime(serial: f64) -> Option<String> {
    // Excel epoch: day 0 = 1899-12-30 (matches Excel's 1900 leap-year bug convention)
    let epoch = chrono::NaiveDate::from_ymd_opt(1899, 12, 30)?.and_hms_opt(0, 0, 0)?;
    let days = serial.trunc() as i64;
    let secs = (serial.fract() * 86_400.0).round() as i64;
    let dt = epoch
        .checked_add_signed(chrono::Duration::days(days))?
        .checked_add_signed(chrono::Duration::seconds(secs))?;
    Some(dt.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// Make header names unique by appending _2, _3, ... to duplicates.
fn dedup_headers(headers: &mut Vec<String>) {
    for i in 0..headers.len() {
        let base = headers[i].clone();
        if base.is_empty() {
            continue;
        }
        let mut n = 2;
        while headers[..i].contains(&headers[i]) {
            headers[i] = format!("{}_{}", base, n);
            n += 1;
        }
    }
}

// --- SQLite writing ---

/// Import parsed sheets into a fresh SQLite database (existing file replaced).
/// Creates one table per sheet plus a `_jcowork_meta` bookkeeping table, and
/// a plain index on every column of every table.
pub async fn import_sheets(
    db_path: &Path,
    sheets: &[SheetData],
    source_path: &str,
) -> Result<ImportSummary> {
    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Fresh database on every import so re-uploads are idempotent.
    if db_path.exists() {
        tokio::fs::remove_file(db_path).await?;
    }
    for suffix in ["-wal", "-shm"] {
        let mut p = db_path.as_os_str().to_owned();
        p.push(suffix);
        let _ = tokio::fs::remove_file(PathBuf::from(p)).await;
    }

    let url = format!("sqlite:{}?mode=rwc", db_path.display());
    let options = SqliteConnectOptions::from_str(&url)?
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;

    sqlx::query("CREATE TABLE _jcowork_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO _jcowork_meta (key, value) VALUES ('source_path', ?1), ('imported_at', ?2)")
        .bind(source_path)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&pool)
        .await?;

    let mut tables = Vec::new();

    for (ti, sheet) in sheets.iter().enumerate() {
        let types = infer_column_types(sheet);
        let cols_def: Vec<String> = sheet
            .headers
            .iter()
            .zip(types.iter())
            .map(|(h, ty)| format!("{} {}", quote_ident(h), ty))
            .collect();
        let create_sql = format!(
            "CREATE TABLE {} ({})",
            quote_ident(&sheet.name),
            cols_def.join(", ")
        );
        sqlx::query(&create_sql)
            .execute(&pool)
            .await
            .with_context(|| format!("Failed to create table for sheet {}", sheet.name))?;

        // Insert rows in batches inside one transaction.
        let mut tx = pool.begin().await?;
        let ncols = sheet.headers.len().max(1);
        let batch = (20_000 / ncols).clamp(1, 1000);
        let col_list: Vec<String> = sheet.headers.iter().map(|h| quote_ident(h)).collect();
        for chunk in sheet.rows.chunks(batch) {
            let mut qb = sqlx::QueryBuilder::new(format!(
                "INSERT INTO {} ({}) ",
                quote_ident(&sheet.name),
                col_list.join(", ")
            ));
            qb.push_values(chunk, |mut b, row| {
                for (i, ty) in types.iter().enumerate() {
                    match row.get(i).unwrap_or(&CellValue::Null) {
                        CellValue::Null => {
                            b.push_bind(Option::<i64>::None);
                        }
                        CellValue::Int(v) if *ty == "TEXT" => {
                            b.push_bind(v.to_string());
                        }
                        CellValue::Int(v) if *ty == "REAL" => {
                            b.push_bind(*v as f64);
                        }
                        CellValue::Int(v) => {
                            b.push_bind(*v);
                        }
                        CellValue::Real(v) if *ty == "TEXT" => {
                            b.push_bind(format_real(*v));
                        }
                        CellValue::Real(v) => {
                            b.push_bind(*v);
                        }
                        CellValue::Text(s) => {
                            b.push_bind(s.clone());
                        }
                    }
                }
            });
            qb.build().execute(&mut *tx).await?;
        }
        tx.commit().await?;

        // One plain index per column, as required for fast filtering.
        for (ci, h) in sheet.headers.iter().enumerate() {
            let idx_sql = format!(
                "CREATE INDEX \"idx_{}_{}\" ON {} ({})",
                ti,
                ci,
                quote_ident(&sheet.name),
                quote_ident(h)
            );
            sqlx::query(&idx_sql).execute(&pool).await?;
        }

        tables.push(TableSummary {
            sheet_name: sheet.name.clone(),
            table_name: sheet.name.clone(),
            columns: sheet
                .headers
                .iter()
                .cloned()
                .zip(types.iter().map(|t| t.to_string()))
                .collect(),
            row_count: sheet.rows.len(),
            truncated: sheet.truncated,
        });
    }

    pool.close().await;

    info!(
        db = %db_path.display(),
        tables = tables.len(),
        "Excel imported into SQLite"
    );

    Ok(ImportSummary {
        db_name: db_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        source_path: source_path.to_string(),
        tables,
        skipped_sheets: Vec::new(),
    })
}

/// Infer a SQLite column type from all values in a column.
/// TEXT wins over REAL, REAL over INTEGER; all-empty columns become TEXT.
fn infer_column_types(sheet: &SheetData) -> Vec<&'static str> {
    let ncols = sheet.headers.len();
    (0..ncols)
        .map(|c| {
            let mut seen_int = false;
            let mut seen_real = false;
            for row in &sheet.rows {
                match row.get(c) {
                    Some(CellValue::Int(_)) => seen_int = true,
                    Some(CellValue::Real(_)) => seen_real = true,
                    Some(CellValue::Text(_)) => return "TEXT",
                    _ => {}
                }
            }
            if seen_real {
                "REAL"
            } else if seen_int {
                "INTEGER"
            } else {
                "TEXT"
            }
        })
        .collect()
}

/// Format an f64 without a trailing ".0" for integral values.
fn format_real(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{:.0}", v)
    } else {
        v.to_string()
    }
}

// --- High-level orchestration used by the workspace indexer ---

/// Parse a workspace Excel file and (re)build its SQLite database.
pub async fn import_excel(
    data_dir: &str,
    user_id: &str,
    source_rel_path: &str,
    workspace_root: &str,
) -> Result<ImportSummary> {
    let full_path = Path::new(workspace_root).join(source_rel_path);
    if !full_path.exists() {
        bail!("File does not exist: {}", source_rel_path);
    }
    let db_path = db_path_for(data_dir, user_id, source_rel_path);
    let db_name = db_name_for(source_rel_path);

    let (sheets, skipped) = {
        let p = full_path.clone();
        tokio::task::spawn_blocking(move || parse_excel_file(&p)).await??
    };
    if sheets.is_empty() {
        bail!("No importable sheets in {}", source_rel_path);
    }

    let mut summary = import_sheets(&db_path, &sheets, source_rel_path).await?;
    summary.db_name = db_name;
    summary.skipped_sheets = skipped;
    Ok(summary)
}

/// Delete the SQLite database belonging to a source Excel file.
/// Returns true when a database file was removed.
pub async fn remove_db_for(data_dir: &str, user_id: &str, source_rel_path: &str) -> Result<bool> {
    let db_path = db_path_for(data_dir, user_id, source_rel_path);
    if !db_path.exists() {
        return Ok(false);
    }
    tokio::fs::remove_file(&db_path).await?;
    for suffix in ["-wal", "-shm"] {
        let mut p = db_path.as_os_str().to_owned();
        p.push(suffix);
        let _ = tokio::fs::remove_file(PathBuf::from(p)).await;
    }
    info!(db = %db_path.display(), "Removed Excel database");
    Ok(true)
}

/// List all imported Excel databases for a user (relative names + table overview).
pub async fn list_databases(data_dir: &str, user_id: &str) -> Result<Vec<ExcelDbInfo>> {
    let base = excel_db_dir(data_dir, user_id);
    let mut files = Vec::new();
    collect_db_files(&base, &mut files);
    files.sort();

    let mut out = Vec::new();
    for path in files.into_iter().take(50) {
        match read_db_info(&base, &path).await {
            Ok(info) => out.push(info),
            Err(e) => warn!(db = %path.display(), err = %e, "Skipping unreadable Excel database"),
        }
    }
    Ok(out)
}

fn collect_db_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_db_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("db") {
            out.push(p);
        }
    }
}

async fn read_db_info(base: &Path, path: &Path) -> Result<ExcelDbInfo> {
    let rel = path
        .strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let name = rel.strip_suffix(".db").unwrap_or(&rel).to_string();
    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let pool = open_db(path).await?;
    let source_file = sqlx::query_as::<_, (String,)>(
        "SELECT value FROM _jcowork_meta WHERE key = 'source_path'",
    )
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten()
    .map(|r| r.0);
    let imported_at = sqlx::query_as::<_, (String,)>(
        "SELECT value FROM _jcowork_meta WHERE key = 'imported_at'",
    )
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten()
    .map(|r| r.0);

    let table_names = user_table_names(&pool).await?;
    let mut tables = Vec::new();
    for t in table_names {
        let count: (i64,) = sqlx::query_as(&format!("SELECT COUNT(*) FROM {}", quote_ident(&t)))
            .fetch_one(&pool)
            .await?;
        tables.push((t, count.0));
    }
    pool.close().await;

    Ok(ExcelDbInfo {
        name,
        size_bytes,
        source_file,
        imported_at,
        tables,
    })
}

/// Names of user tables (excludes internal `_jcowork_*` tables).
pub async fn user_table_names(pool: &SqlitePool) -> Result<Vec<String>> {
    let rows = sqlx::query_as::<_, (String,)>(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND substr(name, 1, 8) <> '_jcowork' ORDER BY name",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// Full schema (columns, types, row counts, indexes) for every table in a database.
pub async fn describe_database(db_path: &Path) -> Result<Vec<TableInfo>> {
    let pool = open_db(db_path).await?;
    let table_names = user_table_names(&pool).await?;
    let mut out = Vec::new();
    for t in table_names {
        let cols = sqlx::query_as::<_, (String, String)>(
            "SELECT name, type FROM pragma_table_info(?1) ORDER BY cid",
        )
        .bind(&t)
        .fetch_all(&pool)
        .await?;
        let count: (i64,) = sqlx::query_as(&format!("SELECT COUNT(*) FROM {}", quote_ident(&t)))
            .fetch_one(&pool)
            .await?;
        let indexes = sqlx::query_as::<_, (String,)>(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 AND name NOT LIKE 'sqlite_autoindex%' ORDER BY name",
        )
        .bind(&t)
        .fetch_all(&pool)
        .await?;
        out.push(TableInfo {
            name: t,
            columns: cols,
            row_count: count.0,
            indexes: indexes.into_iter().map(|r| r.0).collect(),
        });
    }
    pool.close().await;
    Ok(out)
}

/// A preview of one table: schema plus the first rows, for UI display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TablePreview {
    pub name: String,
    /// (column name, SQL type) pairs.
    pub columns: Vec<(String, String)>,
    pub row_count: i64,
    /// First rows, each a list of JSON values in column order.
    pub rows: Vec<Vec<serde_json::Value>>,
}

/// Read up to `max_rows` rows per table for UI preview.
pub async fn preview_database(db_path: &Path, max_rows: usize) -> Result<Vec<TablePreview>> {
    let pool = open_db(db_path).await?;
    let table_names = user_table_names(&pool).await?;
    let mut out = Vec::new();
    for t in table_names {
        let cols = sqlx::query_as::<_, (String, String)>(
            "SELECT name, type FROM pragma_table_info(?1) ORDER BY cid",
        )
        .bind(&t)
        .fetch_all(&pool)
        .await?;
        let count: (i64,) = sqlx::query_as(&format!("SELECT COUNT(*) FROM {}", quote_ident(&t)))
            .fetch_one(&pool)
            .await?;
        let rows_sql = format!("SELECT * FROM {} LIMIT {}", quote_ident(&t), max_rows);
        let rows = sqlx::query(&rows_sql).fetch_all(&pool).await?;
        let ncols = cols.len();
        let json_rows = rows
            .iter()
            .map(|r| (0..ncols).map(|i| cell_json(r, i)).collect::<Vec<_>>())
            .collect();
        out.push(TablePreview {
            name: t,
            columns: cols,
            row_count: count.0,
            rows: json_rows,
        });
    }
    pool.close().await;
    Ok(out)
}

/// Convert a SQLite cell to a JSON value following its storage class.
fn cell_json(row: &sqlx::sqlite::SqliteRow, i: usize) -> serde_json::Value {
    use sqlx::{Row, ValueRef};
    if row.try_get_raw(i).map(|v| v.is_null()).unwrap_or(true) {
        return serde_json::Value::Null;
    }
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return v.into();
    }
    if let Ok(v) = row.try_get::<f64, _>(i) {
        return serde_json::json!(v);
    }
    if let Ok(v) = row.try_get::<String, _>(i) {
        return v.into();
    }
    if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
        return format!("<blob {} bytes>", v.len()).into();
    }
    serde_json::Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sheets() -> Vec<SheetData> {
        vec![
            SheetData {
                name: "员工表".to_string(),
                headers: vec![
                    "姓名".to_string(),
                    "部门".to_string(),
                    "月薪".to_string(),
                    "年龄".to_string(),
                ],
                rows: vec![
                    vec![
                        CellValue::Text("张三".into()),
                        CellValue::Text("技术".into()),
                        CellValue::Real(15000.5),
                        CellValue::Int(30),
                    ],
                    vec![
                        CellValue::Text("李四".into()),
                        CellValue::Text("市场".into()),
                        CellValue::Real(12000.0),
                        CellValue::Int(28),
                    ],
                    vec![
                        CellValue::Text("王五".into()),
                        CellValue::Null,
                        CellValue::Null,
                        CellValue::Int(45),
                    ],
                ],
                truncated: false,
            },
            SheetData {
                name: "Sheet2".to_string(),
                headers: vec!["code".to_string(), "flag".to_string()],
                rows: vec![vec![CellValue::Int(7), CellValue::Int(1)]],
                truncated: false,
            },
        ]
    }

    #[tokio::test]
    async fn test_import_and_describe() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("sub").join("数据.db");

        let summary = import_sheets(&db_path, &sample_sheets(), "sub/数据.xlsx")
            .await
            .unwrap();
        assert_eq!(summary.tables.len(), 2);
        assert_eq!(summary.tables[0].row_count, 3);
        assert_eq!(
            summary.tables[0].columns,
            vec![
                ("姓名".to_string(), "TEXT".to_string()),
                ("部门".to_string(), "TEXT".to_string()),
                ("月薪".to_string(), "REAL".to_string()),
                ("年龄".to_string(), "INTEGER".to_string()),
            ]
        );

        // describe_database reflects tables, column types and per-column indexes
        let info = describe_database(&db_path).await.unwrap();
        assert_eq!(info.len(), 2);
        let t0 = &info[0];
        assert_eq!(t0.name, "Sheet2"); // ORDER BY name
        let t1 = &info[1];
        assert_eq!(t1.name, "员工表");
        assert_eq!(t1.row_count, 3);
        assert_eq!(t1.columns.len(), 4);
        assert_eq!(t1.indexes.len(), 4); // one index per column

        // NULL handling: query the imported data
        let pool = open_db(&db_path).await.unwrap();
        let row: (Option<String>, Option<f64>) =
            sqlx::query_as("SELECT \"部门\", \"月薪\" FROM \"员工表\" WHERE \"姓名\" = '王五'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0, None);
        assert_eq!(row.1, None);
        pool.close().await;

        // meta table recorded the source
        let pool = open_db(&db_path).await.unwrap();
        let src: (String,) =
            sqlx::query_as("SELECT value FROM _jcowork_meta WHERE key = 'source_path'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(src.0, "sub/数据.xlsx");
        pool.close().await;
    }

    #[tokio::test]
    async fn test_reimport_replaces_database() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("a.db");
        import_sheets(&db_path, &sample_sheets(), "a.xlsx")
            .await
            .unwrap();
        // Re-import with a single sheet: old table must disappear.
        let sheets = vec![sample_sheets().into_iter().nth(1).unwrap()];
        let summary = import_sheets(&db_path, &sheets, "a.xlsx").await.unwrap();
        assert_eq!(summary.tables.len(), 1);
        let info = describe_database(&db_path).await.unwrap();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].name, "Sheet2");
    }

    #[tokio::test]
    async fn test_list_and_remove_databases() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_str().unwrap().to_string();
        let user = "u1";

        let db_path = db_path_for(&data_dir, user, "reports/销售.xlsx");
        import_sheets(&db_path, &sample_sheets(), "reports/销售.xlsx")
            .await
            .unwrap();

        let dbs = list_databases(&data_dir, user).await.unwrap();
        assert_eq!(dbs.len(), 1);
        assert_eq!(dbs[0].name, "reports/销售");
        assert_eq!(dbs[0].source_file.as_deref(), Some("reports/销售.xlsx"));
        assert_eq!(dbs[0].tables.len(), 2);

        assert!(remove_db_for(&data_dir, user, "reports/销售.xlsx").await.unwrap());
        assert!(list_databases(&data_dir, user).await.unwrap().is_empty());
        assert!(!remove_db_for(&data_dir, user, "reports/销售.xlsx").await.unwrap());
    }

    #[tokio::test]
    async fn test_resolve_db_path_validation() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_str().unwrap().to_string();
        let db_path = db_path_for(&data_dir, "u1", "a.xlsx");
        import_sheets(&db_path, &sample_sheets(), "a.xlsx").await.unwrap();

        assert!(resolve_db_path(&data_dir, "u1", "a").await.is_ok());
        assert!(resolve_db_path(&data_dir, "u1", "a.db").await.is_ok());
        assert!(resolve_db_path(&data_dir, "u1", "../a").await.is_err());
        assert!(resolve_db_path(&data_dir, "u1", "/etc/a").await.is_err());
        assert!(resolve_db_path(&data_dir, "u1", "missing").await.is_err());
    }

    #[test]
    fn test_dedup_headers() {
        let mut h = vec!["a".to_string(), "a".to_string(), "b".to_string(), "a".to_string()];
        dedup_headers(&mut h);
        assert_eq!(h, vec!["a", "a_2", "b", "a_3"]);
    }

    #[test]
    fn test_db_name_for() {
        assert_eq!(db_name_for("销售.xlsx"), "销售");
        assert_eq!(db_name_for("reports/2024/数据.xls"), "reports/2024/数据");
    }

    #[tokio::test]
    async fn test_parse_real_xlsx() {
        use rust_xlsxwriter::Workbook;

        let dir = tempfile::tempdir().unwrap();
        let xlsx_path = dir.path().join("test.xlsx");

        let mut wb = Workbook::new();
        let ws = wb.add_worksheet().set_name("销售").unwrap();
        ws.write_string(0, 0, "产品").unwrap();
        ws.write_string(0, 1, "数量").unwrap();
        ws.write_string(0, 2, "单价").unwrap();
        ws.write_string(1, 0, "苹果").unwrap();
        ws.write_number(1, 1, 10).unwrap();
        ws.write_number(1, 2, 3.5).unwrap();
        ws.write_string(2, 0, "香蕉").unwrap();
        ws.write_number(2, 1, 5).unwrap();
        ws.write_number(2, 2, 2.0).unwrap();
        // duplicate headers
        ws.write_string(0, 3, "产品").unwrap();
        ws.write_string(1, 3, "dup").unwrap();
        // empty sheet should be skipped
        wb.add_worksheet().set_name("空表").unwrap();
        wb.save(&xlsx_path).unwrap();

        let (sheets, skipped) = parse_excel_file(&xlsx_path).unwrap();
        assert_eq!(sheets.len(), 1);
        assert_eq!(skipped, vec!["空表".to_string()]);

        let s = &sheets[0];
        assert_eq!(s.name, "销售");
        assert_eq!(s.headers, vec!["产品", "数量", "单价", "产品_2"]);
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.rows[0][0], CellValue::Text("苹果".into()));
        assert_eq!(s.rows[0][1], CellValue::Real(10.0)); // xlsx numbers arrive as Float
        assert_eq!(s.rows[0][2], CellValue::Real(3.5));

        // End-to-end through import_sheets: REAL column holds 10.0 / 5.0
        let db_path = dir.path().join("test.db");
        let summary = import_sheets(&db_path, &sheets, "test.xlsx").await.unwrap();
        let qty_col = summary.tables[0]
            .columns
            .iter()
            .find(|(n, _)| n == "数量")
            .unwrap();
        assert_eq!(qty_col.1, "REAL");
    }

    #[tokio::test]
    async fn test_preview_database() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("a.db");
        import_sheets(&db_path, &sample_sheets(), "a.xlsx")
            .await
            .unwrap();

        let preview = preview_database(&db_path, 2).await.unwrap();
        assert_eq!(preview.len(), 2);

        // ORDER BY name: Sheet2 first, then 员工表
        let t = &preview[1];
        assert_eq!(t.name, "员工表");
        assert_eq!(t.row_count, 3);
        // limited to 2 rows
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.columns.len(), 4);
        // JSON cell types follow SQLite storage classes
        assert_eq!(t.rows[0][0], serde_json::json!("张三"));
        assert_eq!(t.rows[0][2], serde_json::json!(15000.5));
        assert_eq!(t.rows[0][3], serde_json::json!(30));

        // NULL cells become JSON null
        let full = preview_database(&db_path, 10).await.unwrap();
        assert_eq!(full[1].rows[2][1], serde_json::Value::Null);
    }
}
