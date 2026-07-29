//! excel_db tool — CRUD over the SQLite databases imported from uploaded Excel files.
//!
//! When a user uploads an Excel file (.xlsx/.xls) it is parsed into a SQLite
//! database under `{data_dir}/{user_id}/excel_db/` (see
//! `jcowork_storage::excel_db`). This tool lets the agent list those
//! databases, inspect their schemas, run read-only SELECT queries, and
//! insert/update/delete rows.

use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{Column, Row, ValueRef};

use crate::base::{Tool, ToolContext};
use jcowork_storage::excel_db;

/// CRUD tool for Excel-imported SQLite databases.
pub struct ExcelDbTool;

#[derive(Debug, Deserialize)]
struct ExcelDbArgs {
    /// "list" | "query" | "insert" | "update" | "delete"
    action: String,
    /// Database name (relative, without .db) as shown by the list action.
    db: Option<String>,
    /// Table name (equals the Excel sheet name).
    table: Option<String>,
    /// SELECT statement for the query action.
    sql: Option<String>,
    /// Rows for the insert action.
    rows: Option<Vec<serde_json::Map<String, Value>>>,
    /// Column/value pairs for the update action.
    set: Option<serde_json::Map<String, Value>>,
    /// WHERE condition (without the keyword) for update/delete.
    #[serde(rename = "where")]
    where_clause: Option<String>,
    /// Row cap for the query action (default 100, max 1000).
    limit: Option<u32>,
}

#[async_trait]
impl Tool for ExcelDbTool {
    fn name(&self) -> &str {
        "excel_db"
    }

    fn description(&self) -> &str {
        "Query and manage data from uploaded Excel files. Each uploaded .xlsx/.xls is parsed into a SQLite database (one table per worksheet, every column indexed). Actions: \"list\" (no db: all databases; with db: full schema), \"query\" (read-only SELECT), \"insert\" (rows), \"update\" (set + where), \"delete\" (where required)."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "query", "insert", "update", "delete"],
                    "description": "Operation to perform: list databases/schemas, query rows, insert rows, update rows, delete rows"
                },
                "db": {
                    "type": "string",
                    "description": "Database name as shown by the list action (relative name without .db). Required for query/insert/update/delete; optional for list"
                },
                "table": {
                    "type": "string",
                    "description": "Table name (equals the Excel worksheet name, may contain Chinese). Required for insert/update/delete"
                },
                "sql": {
                    "type": "string",
                    "description": "A single read-only SELECT statement (WITH ... SELECT is allowed). Quote identifiers with double quotes. Required for query"
                },
                "rows": {
                    "type": "array",
                    "items": { "type": "object" },
                    "description": "Rows to insert; each item maps column names to values. Required for insert"
                },
                "set": {
                    "type": "object",
                    "description": "Column/value pairs to set. Required for update"
                },
                "where": {
                    "type": "string",
                    "description": "SQL WHERE condition WITHOUT the keyword (e.g. \"部门\" = '技术'). Required for update/delete as a safety guard"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum rows returned by query (default 100, max 1000)",
                    "default": 100
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<String> {
        let parsed: ExcelDbArgs = serde_json::from_str(args)?;
        let data_dir = data_dir_from_ctx(ctx)?;

        match parsed.action.as_str() {
            "list" => self.action_list(&data_dir, ctx, parsed.db.as_deref()).await,
            "query" => {
                let db = require(parsed.db.as_deref(), "db")?;
                let sql = require(parsed.sql.as_deref(), "sql")?;
                let path = excel_db::resolve_db_path(&data_dir, &ctx.user_id, db).await?;
                let pool = excel_db::open_db(&path).await?;
                let result = self.action_query(&pool, sql, parsed.limit).await;
                pool.close().await;
                result
            }
            "insert" => {
                let db = require(parsed.db.as_deref(), "db")?;
                let table = require(parsed.table.as_deref(), "table")?;
                let rows = parsed
                    .rows
                    .as_ref()
                    .filter(|r| !r.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("\"rows\" is required for insert and must be a non-empty array of objects"))?;
                let path = excel_db::resolve_db_path(&data_dir, &ctx.user_id, db).await?;
                let pool = excel_db::open_db(&path).await?;
                let result = self.action_insert(&pool, table, rows).await;
                pool.close().await;
                result
            }
            "update" => {
                let db = require(parsed.db.as_deref(), "db")?;
                let table = require(parsed.table.as_deref(), "table")?;
                let set = parsed
                    .set
                    .as_ref()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("\"set\" is required for update and must be a non-empty object"))?;
                let where_clause = require_where(parsed.where_clause.as_deref())?;
                let path = excel_db::resolve_db_path(&data_dir, &ctx.user_id, db).await?;
                let pool = excel_db::open_db(&path).await?;
                let result = self.action_update(&pool, table, set, where_clause).await;
                pool.close().await;
                result
            }
            "delete" => {
                let db = require(parsed.db.as_deref(), "db")?;
                let table = require(parsed.table.as_deref(), "table")?;
                let where_clause = require_where(parsed.where_clause.as_deref())?;
                let path = excel_db::resolve_db_path(&data_dir, &ctx.user_id, db).await?;
                let pool = excel_db::open_db(&path).await?;
                let result = self.action_delete(&pool, table, where_clause).await;
                pool.close().await;
                result
            }
            other => bail!(
                "Unknown action \"{}\". Supported actions: list, query, insert, update, delete.",
                other
            ),
        }
    }
}

impl ExcelDbTool {
    async fn action_list(&self, data_dir: &str, ctx: &ToolContext, db: Option<&str>) -> Result<String> {
        match db {
            None => {
                let dbs = excel_db::list_databases(data_dir, &ctx.user_id).await?;
                if dbs.is_empty() {
                    return Ok("No Excel databases found. Upload an Excel file (.xlsx/.xls) through the Documents page — it is parsed into a SQLite database automatically.".to_string());
                }
                let mut out = format!("Excel databases ({}):\n", dbs.len());
                for d in &dbs {
                    out.push_str(&format!("\n- db: \"{}\" ({} bytes", d.name, d.size_bytes));
                    if let Some(src) = &d.source_file {
                        out.push_str(&format!(", source: {}", src));
                    }
                    if let Some(at) = &d.imported_at {
                        out.push_str(&format!(", imported: {}", at));
                    }
                    out.push_str(")\n  Tables: ");
                    let tables: Vec<String> = d
                        .tables
                        .iter()
                        .map(|(t, n)| format!("\"{}\" ({} rows)", t, n))
                        .collect();
                    out.push_str(&tables.join(", "));
                    out.push_str(&format!(
                        "\n  For full schema: action=\"list\", db=\"{}\"\n",
                        d.name
                    ));
                }
                Ok(out.trim_end().to_string())
            }
            Some(name) => {
                let path = excel_db::resolve_db_path(data_dir, &ctx.user_id, name).await?;
                let tables = excel_db::describe_database(&path).await?;
                if tables.is_empty() {
                    return Ok(format!("Database \"{}\" has no tables.", name));
                }
                let mut out = format!("Schema of Excel database \"{}\":", name);
                for t in &tables {
                    out.push_str(&format!(
                        "\n\nTable \"{}\" — {} rows, {} index(es)\n",
                        t.name,
                        t.row_count,
                        t.indexes.len()
                    ));
                    for (c, ty) in &t.columns {
                        out.push_str(&format!("  - \"{}\" {}\n", c, ty));
                    }
                }
                Ok(out.trim_end().to_string())
            }
        }
    }

    async fn action_query(&self, pool: &SqlitePool, sql: &str, limit: Option<u32>) -> Result<String> {
        let cleaned = validate_select_sql(sql)?;
        let limit = limit.unwrap_or(100).clamp(1, 1000) as usize;
        // Fetch one extra row to detect truncation; the wrapper caps memory usage.
        let wrapped = format!("SELECT * FROM ({}) LIMIT {}", cleaned, limit + 1);
        let mut rows = sqlx::query(&wrapped).fetch_all(pool).await?;
        let truncated = rows.len() > limit;
        if truncated {
            rows.truncate(limit);
        }
        Ok(render_rows(&rows, truncated))
    }

    async fn action_insert(
        &self,
        pool: &SqlitePool,
        table: &str,
        rows: &[serde_json::Map<String, Value>],
    ) -> Result<String> {
        let table_cols = table_columns(pool, table).await?;

        // Union of all keys, first-seen order.
        let mut cols: Vec<String> = Vec::new();
        for row in rows {
            for (k, v) in row {
                if v.is_array() || v.is_object() {
                    bail!("Unsupported nested value for column \"{}\" — only strings, numbers, booleans and null are allowed", k);
                }
                if !cols.contains(k) {
                    cols.push(k.clone());
                }
            }
        }
        for c in &cols {
            if !table_cols.contains(c) {
                bail!(
                    "Unknown column \"{}\" in table \"{}\". Available columns: {}",
                    c,
                    table,
                    table_cols.join(", ")
                );
            }
        }

        let col_list: Vec<String> = cols.iter().map(|c| excel_db::quote_ident(c)).collect();
        let batch = (20_000 / cols.len().max(1)).clamp(1, 1000);
        let mut total = 0u64;
        let mut tx = pool.begin().await?;
        for chunk in rows.chunks(batch) {
            let mut qb = sqlx::QueryBuilder::new(format!(
                "INSERT INTO {} ({}) ",
                excel_db::quote_ident(table),
                col_list.join(", ")
            ));
            qb.push_values(chunk, |mut b, row| {
                for c in &cols {
                    match row.get(c) {
                        None | Some(Value::Null) => {
                            b.push_bind(Option::<String>::None);
                        }
                        Some(Value::Bool(v)) => {
                            b.push_bind(*v as i64);
                        }
                        Some(Value::Number(n)) => {
                            if let Some(i) = n.as_i64() {
                                b.push_bind(i);
                            } else {
                                b.push_bind(n.as_f64().unwrap_or(0.0));
                            }
                        }
                        Some(Value::String(s)) => {
                            b.push_bind(s.clone());
                        }
                        // Pre-validated above: arrays/objects rejected.
                        Some(_) => {
                            b.push_bind(Option::<String>::None);
                        }
                    }
                }
            });
            total += qb.build().execute(&mut *tx).await?.rows_affected();
        }
        tx.commit().await?;
        Ok(format!("OK: inserted {} row(s) into \"{}\".", total, table))
    }

    async fn action_update(
        &self,
        pool: &SqlitePool,
        table: &str,
        set: &serde_json::Map<String, Value>,
        where_clause: &str,
    ) -> Result<String> {
        let table_cols = table_columns(pool, table).await?;
        for (c, v) in set {
            if v.is_array() || v.is_object() {
                bail!("Unsupported nested value for column \"{}\"", c);
            }
            if !table_cols.contains(c) {
                bail!(
                    "Unknown column \"{}\" in table \"{}\". Available columns: {}",
                    c,
                    table,
                    table_cols.join(", ")
                );
            }
        }

        let mut qb = sqlx::QueryBuilder::new(format!("UPDATE {} SET ", excel_db::quote_ident(table)));
        {
            let mut sep = qb.separated(", ");
            for (c, v) in set {
                let frag = sep.push(format!("{} = ", excel_db::quote_ident(c)));
                match v {
                    Value::Null => {
                        frag.push_bind_unseparated(Option::<String>::None);
                    }
                    Value::Bool(b) => {
                        frag.push_bind_unseparated(*b as i64);
                    }
                    Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            frag.push_bind_unseparated(i);
                        } else {
                            frag.push_bind_unseparated(n.as_f64().unwrap_or(0.0));
                        }
                    }
                    Value::String(s) => {
                        frag.push_bind_unseparated(s.clone());
                    }
                    // Pre-validated above.
                    _ => {
                        frag.push_bind_unseparated(Option::<String>::None);
                    }
                }
            }
        }
        qb.push(" WHERE ");
        qb.push(where_clause);

        let res = qb.build().execute(pool).await?;
        Ok(format!(
            "OK: {} row(s) updated in \"{}\".",
            res.rows_affected(),
            table
        ))
    }

    async fn action_delete(&self, pool: &SqlitePool, table: &str, where_clause: &str) -> Result<String> {
        let _ = table_columns(pool, table).await?;
        let sql = format!(
            "DELETE FROM {} WHERE {}",
            excel_db::quote_ident(table),
            where_clause
        );
        let res = sqlx::query(&sql).execute(pool).await?;
        Ok(format!(
            "OK: {} row(s) deleted from \"{}\".",
            res.rows_affected(),
            table
        ))
    }
}

/// Derive `{data_dir}` from `ctx.workspace_root` (= `{data_dir}/{user_id}/workspace`).
fn data_dir_from_ctx(ctx: &ToolContext) -> Result<String> {
    let workspace_path = std::path::Path::new(&ctx.workspace_root);
    workspace_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| anyhow::anyhow!("Cannot determine data_dir from workspace_root"))
}

fn require<'a>(v: Option<&'a str>, name: &str) -> Result<&'a str> {
    v.filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("\"{}\" is required for this action", name))
}

/// WHERE clause is mandatory for update/delete (guards against wiping tables).
fn require_where(v: Option<&str>) -> Result<&str> {
    let w = require(v, "where")?;
    if w.contains(';') {
        bail!("The where condition must not contain ';'");
    }
    Ok(w)
}

/// Column names of an existing user table; rejects internal `_jcowork_*` tables.
async fn table_columns(pool: &SqlitePool, table: &str) -> Result<Vec<String>> {
    if table.starts_with("_jcowork") {
        bail!("Access to internal table \"{}\" is not allowed", table);
    }
    let rows = sqlx::query_as::<_, (String,)>("SELECT name FROM pragma_table_info(?1)")
        .bind(table)
        .fetch_all(pool)
        .await?;
    if rows.is_empty() {
        bail!(
            "Table \"{}\" not found. Use action=\"list\" with the db name to see available tables.",
            table
        );
    }
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// Validate that `sql` is a single read-only SELECT statement and return it
/// trimmed (trailing semicolon removed). Literals, quoted identifiers and
/// comments are stripped before keyword checks so words like "update" inside
/// a string don't cause false rejections.
fn validate_select_sql(sql: &str) -> Result<String> {
    let trimmed = sql.trim().trim_end_matches(';').trim().to_string();
    if trimmed.is_empty() {
        bail!("\"sql\" must not be empty");
    }
    let stripped = strip_literals_and_comments(&trimmed).to_lowercase();
    if stripped.contains(';') {
        bail!("Only a single statement is allowed");
    }
    let head = stripped.trim_start();
    if !(head.starts_with("select") || head.starts_with("with")) {
        bail!("Only read-only SELECT queries are allowed");
    }
    // Guard against write statements hiding behind a WITH clause
    // (SQLite allows WITH ... INSERT/UPDATE/DELETE).
    for word in stripped.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        match word {
            "insert" | "update" | "delete" | "drop" | "alter" | "attach" | "detach" | "pragma"
            | "vacuum" | "reindex" | "create" => {
                bail!("Only read-only SELECT queries are allowed (found keyword \"{}\")", word);
            }
            _ => {}
        }
    }
    Ok(trimmed)
}

/// Replace string literals, quoted identifiers and comments with spaces.
fn strip_literals_and_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' | '`' => {
                let quote = c;
                out.push(' ');
                while let Some(inner) = chars.next() {
                    if inner == quote {
                        // '' / "" / `` escape the quote inside itself
                        if chars.peek() == Some(&quote) {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                for inner in chars.by_ref() {
                    if inner == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                let mut prev = '\0';
                for inner in chars.by_ref() {
                    if prev == '*' && inner == '/' {
                        break;
                    }
                    prev = inner;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Render query rows as a plain-text table, capping cell and total size.
fn render_rows(rows: &[SqliteRow], truncated: bool) -> String {
    if rows.is_empty() {
        return "Query OK: 0 rows.".to_string();
    }
    let cols: Vec<String> = rows[0]
        .columns()
        .iter()
        .map(|c| c.name().to_string())
        .collect();

    let mut out = String::new();
    out.push_str(&cols.join(" | "));
    for r in rows {
        out.push('\n');
        let cells: Vec<String> = (0..cols.len()).map(|i| cell_text(r, i)).collect();
        out.push_str(&cells.join(" | "));
    }

    const MAX_OUT: usize = 12_000;
    if out.len() > MAX_OUT {
        let mut end = MAX_OUT;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str("\n[... output truncated ...]");
    }

    format!(
        "{} row(s){}:\n{}",
        rows.len(),
        if truncated { " (result capped, use limit or refine the query)" } else { "" },
        out
    )
}

/// Best-effort text rendering of a single cell, following SQLite storage classes.
fn cell_text(row: &SqliteRow, i: usize) -> String {
    if row.try_get_raw(i).map(|v| v.is_null()).unwrap_or(true) {
        return "NULL".to_string();
    }
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return v.to_string();
    }
    if let Ok(v) = row.try_get::<f64, _>(i) {
        return v.to_string();
    }
    if let Ok(v) = row.try_get::<String, _>(i) {
        const MAX_CELL: usize = 80;
        if v.len() > MAX_CELL {
            let mut end = MAX_CELL;
            while end > 0 && !v.is_char_boundary(end) {
                end -= 1;
            }
            return format!("{}…", &v[..end]);
        }
        return v;
    }
    if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
        return format!("<blob {} bytes>", v.len());
    }
    "?".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcowork_storage::excel_db::{CellValue, SheetData, db_path_for, import_sheets};

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        // Layout: dir/data_dir/test-user/workspace
        ToolContext {
            user_id: "test-user".to_string(),
            workspace_root: dir
                .join("data_dir")
                .join("test-user")
                .join("workspace")
                .to_string_lossy()
                .to_string(),
        }
    }

    fn sample_sheets() -> Vec<SheetData> {
        vec![SheetData {
            name: "员工表".to_string(),
            headers: vec!["姓名".to_string(), "部门".to_string(), "月薪".to_string()],
            rows: vec![
                vec![
                    CellValue::Text("张三".into()),
                    CellValue::Text("技术".into()),
                    CellValue::Real(15000.5),
                ],
                vec![
                    CellValue::Text("李四".into()),
                    CellValue::Text("市场".into()),
                    CellValue::Real(12000.0),
                ],
            ],
            truncated: false,
        }]
    }

    async fn seed(dir: &std::path::Path) -> ToolContext {
        let ctx = make_ctx(dir);
        tokio::fs::create_dir_all(&ctx.workspace_root).await.unwrap();
        let data_dir = data_dir_from_ctx(&ctx).unwrap();
        let db_path = db_path_for(&data_dir, &ctx.user_id, "test.xlsx");
        import_sheets(&db_path, &sample_sheets(), "test.xlsx")
            .await
            .unwrap();
        ctx
    }

    #[test]
    fn test_validate_select_sql_accepts_reads() {
        assert!(validate_select_sql("select 1").is_ok());
        assert!(validate_select_sql("  SELECT * FROM \"员工表\" WHERE \"月薪\" > 100;  ").is_ok());
        assert!(validate_select_sql("WITH x AS (SELECT 1 AS a) SELECT * FROM x").is_ok());
        // keywords inside literals/identifiers/comments must not false-trigger
        assert!(validate_select_sql("select 'drop table x' from t").is_ok());
        assert!(validate_select_sql("select \"update\" from t -- delete from t").is_ok());
        assert!(validate_select_sql("select 'a;b' from t").is_ok());
        assert!(validate_select_sql("select replace(name, 'a', 'b') from t").is_ok());
    }

    #[test]
    fn test_validate_select_sql_rejects_writes() {
        assert!(validate_select_sql("delete from t").is_err());
        assert!(validate_select_sql("update t set a = 1").is_err());
        assert!(validate_select_sql("insert into t values (1)").is_err());
        assert!(validate_select_sql("drop table t").is_err());
        assert!(validate_select_sql("select 1; drop table t").is_err());
        assert!(validate_select_sql("with x as (select 1) delete from t").is_err());
        assert!(validate_select_sql("pragma table_info(t)").is_err());
        assert!(validate_select_sql("").is_err());
    }

    #[tokio::test]
    async fn test_list_empty() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_ctx(dir.path());
        tokio::fs::create_dir_all(&ctx.workspace_root).await.unwrap();
        let result = ExcelDbTool.execute(r#"{"action":"list"}"#, &ctx).await.unwrap();
        assert!(result.contains("No Excel databases"));
    }

    #[tokio::test]
    async fn test_list_and_schema() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = seed(dir.path()).await;

        let out = ExcelDbTool.execute(r#"{"action":"list"}"#, &ctx).await.unwrap();
        assert!(out.contains("\"test\""));
        assert!(out.contains("员工表"));
        assert!(out.contains("2 rows"));

        let out = ExcelDbTool
            .execute(r#"{"action":"list","db":"test"}"#, &ctx)
            .await
            .unwrap();
        assert!(out.contains("Table \"员工表\""));
        assert!(out.contains("\"月薪\" REAL"));
        assert!(out.contains("3 index(es)"));
    }

    #[tokio::test]
    async fn test_query() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = seed(dir.path()).await;

        let out = ExcelDbTool
            .execute(
                r#"{"action":"query","db":"test","sql":"SELECT \"姓名\", \"月薪\" FROM \"员工表\" WHERE \"月薪\" > 13000"}"#,
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("1 row(s)"));
        assert!(out.contains("张三"));
        assert!(out.contains("15000.5"));
        assert!(!out.contains("李四"));

        // write statements rejected
        let err = ExcelDbTool
            .execute(r#"{"action":"query","db":"test","sql":"DELETE FROM \"员工表\""}"#, &ctx)
            .await;
        assert!(err.is_err());

        // missing db
        let err = ExcelDbTool
            .execute(r#"{"action":"query","db":"nope","sql":"select 1"}"#, &ctx)
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_insert_update_delete_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = seed(dir.path()).await;

        // insert
        let out = ExcelDbTool
            .execute(
                r#"{"action":"insert","db":"test","table":"员工表","rows":[{"姓名":"王五","部门":"技术","月薪":18000},{"姓名":"赵六","部门":"市场"}]}"#,
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("inserted 2 row(s)"));

        // update
        let out = ExcelDbTool
            .execute(
                r#"{"action":"update","db":"test","table":"员工表","set":{"月薪":19000},"where":"\"姓名\" = '王五'"}"#,
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("1 row(s) updated"));

        // delete
        let out = ExcelDbTool
            .execute(
                r#"{"action":"delete","db":"test","table":"员工表","where":"\"姓名\" = '赵六'"}"#,
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("1 row(s) deleted"));

        // final state: 张三, 李四, 王五(19000)
        let out = ExcelDbTool
            .execute(
                r#"{"action":"query","db":"test","sql":"SELECT \"姓名\", \"月薪\" FROM \"员工表\" ORDER BY \"月薪\" DESC","limit":10}"#,
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("3 row(s)"));
        assert!(out.contains("19000"));
        assert!(!out.contains("赵六"));
    }

    #[tokio::test]
    async fn test_mutation_guards() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = seed(dir.path()).await;

        // update/delete require where
        assert!(ExcelDbTool
            .execute(r#"{"action":"update","db":"test","table":"员工表","set":{"月薪":1}}"#, &ctx)
            .await
            .is_err());
        assert!(ExcelDbTool
            .execute(r#"{"action":"delete","db":"test","table":"员工表"}"#, &ctx)
            .await
            .is_err());
        // unknown column
        assert!(ExcelDbTool
            .execute(r#"{"action":"insert","db":"test","table":"员工表","rows":[{"不存在":1}]}"#, &ctx)
            .await
            .is_err());
        // unknown table
        assert!(ExcelDbTool
            .execute(r#"{"action":"delete","db":"test","table":"ghost","where":"1=1"}"#, &ctx)
            .await
            .is_err());
        // internal table blocked
        assert!(ExcelDbTool
            .execute(r#"{"action":"delete","db":"test","table":"_jcowork_meta","where":"1=1"}"#, &ctx)
            .await
            .is_err());
    }
}
