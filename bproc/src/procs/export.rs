//! CSV / JSON export of the current table view.
use super::columns::{ColumnCtx, ColumnId};
use super::store::{ProcessStore, Row, ViewRow};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
}

/// Render `rows` with `columns` in `format`.
///
/// Exactly the visible columns are exported, prefixed by PID when it is not
/// among them. CSV (RFC 4180, CRLF line endings) has a header of column
/// titles and the cell text as displayed. JSON is an array of objects keyed by
/// column filter key in column order: numbers where the column has a numeric
/// value, `null` for a missing numeric value, strings otherwise, plus `depth`
/// when the rows come from tree mode. Rows no longer in the store are skipped.
pub fn render(
    store: &ProcessStore,
    rows: &[ViewRow],
    columns: &[ColumnId],
    ctx: &ColumnCtx,
    format: ExportFormat,
) -> String {
    let columns = export_columns(columns);
    let tree = rows
        .iter()
        .any(|r| r.depth > 0 || r.has_children || !r.tree_prefix.is_empty());
    let resolved = rows
        .iter()
        .filter_map(|v| store.get(&v.key).map(|row| (v, row)));
    match format {
        ExportFormat::Csv => csv(resolved, &columns, ctx),
        ExportFormat::Json => json(resolved, &columns, ctx, tree),
    }
}

fn export_columns(columns: &[ColumnId]) -> Vec<ColumnId> {
    let mut out = Vec::with_capacity(columns.len() + 1);
    if !columns.contains(&ColumnId::Pid) {
        out.push(ColumnId::Pid);
    }
    for &c in columns {
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

fn csv<'a>(
    rows: impl Iterator<Item = (&'a ViewRow, &'a Row)>,
    columns: &[ColumnId],
    ctx: &ColumnCtx,
) -> String {
    let mut out = String::new();
    let header: Vec<String> = columns.iter().map(|c| csv_field(c.title())).collect();
    out.push_str(&header.join(","));
    out.push_str("\r\n");
    for (_, row) in rows {
        let cells: Vec<String> = columns
            .iter()
            .map(|&c| {
                let text = c.text(row, ctx);
                csv_field(&if c.is_text() { defuse(text) } else { text })
            })
            .collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    }
    out
}

/// Quote a CSV field when it contains a delimiter, quote or line break.
fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// Neutralise text a spreadsheet would evaluate as a formula (process names
/// and command lines are attacker controlled). `-` is left alone so login
/// shells such as `-zsh` keep their names.
fn defuse(text: String) -> String {
    if text.starts_with(['=', '+', '@', '\t', '\r']) {
        format!("'{text}")
    } else {
        text
    }
}

fn json<'a>(
    rows: impl Iterator<Item = (&'a ViewRow, &'a Row)>,
    columns: &[ColumnId],
    ctx: &ColumnCtx,
    tree: bool,
) -> String {
    let objects: Vec<String> = rows
        .map(|(view, row)| {
            let mut fields: Vec<String> = columns
                .iter()
                .map(|&c| {
                    let value = match c.numeric(row, ctx) {
                        Some(v) => number(v),
                        None if !c.is_text() => Value::Null,
                        None => Value::String(c.text(row, ctx)),
                    };
                    format!("{}: {value}", Value::from(c.filter_key()))
                })
                .collect();
            if tree {
                fields.push(format!("\"depth\": {}", view.depth));
            }
            format!("  {{{}}}", fields.join(", "))
        })
        .collect();
    if objects.is_empty() {
        "[]\n".into()
    } else {
        format!("[\n{}\n]\n", objects.join(",\n"))
    }
}

/// Integers stay integers; fractions are rounded to three places so f32
/// noise (0.100000001) does not leak into the file.
fn number(v: f64) -> Value {
    const EXACT: f64 = 9_007_199_254_740_992.0;
    if !v.is_finite() {
        Value::Null
    } else if v.fract() == 0.0 && v.abs() < EXACT {
        Value::from(v as i64)
    } else {
        serde_json::Number::from_f64((v * 1000.0).round() / 1000.0)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

/// Write an export to `dir` (default: current directory) and return the path.
pub fn write(
    contents: &str,
    format: ExportFormat,
    dir: Option<&std::path::Path>,
) -> std::io::Result<std::path::PathBuf> {
    let name = format!(
        "bproc-{}.{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        match format {
            ExportFormat::Csv => "csv",
            ExportFormat::Json => "json",
        }
    );
    let path = dir
        .map(|d| d.join(&name))
        .unwrap_or_else(|| std::path::PathBuf::from(&name));
    std::fs::write(&path, contents)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::model::SystemSnapshot;
    use crate::procs::model::{ProcessEntry, ProcessKey, ProcessRecord, ProcessSample};
    use std::sync::Arc;

    fn entry(pid: i32, name: &str, cmdline: &[&str], cpu: f32) -> ProcessEntry {
        ProcessEntry {
            record: Arc::new(ProcessRecord {
                key: ProcessKey::new(pid, 10),
                ppid: (pid > 1).then_some(1),
                name: name.into(),
                cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
                user: Some("root".into()),
                ..Default::default()
            }),
            sample: ProcessSample {
                cpu_percent: cpu,
                mem_rss: 2048,
                ..Default::default()
            },
        }
    }

    fn view(key: ProcessKey, depth: u16) -> ViewRow {
        ViewRow {
            key,
            depth,
            has_children: false,
            collapsed: false,
            tree_prefix: String::new(),
            pinned: false,
            ghost: false,
            highlights: Vec::new(),
            aggregate: None,
        }
    }

    fn fixture() -> (ProcessStore, Vec<ViewRow>) {
        let store = ProcessStore::from_entries(
            vec![
                entry(1, "init", &[], 0.1),
                entry(2, "we,ird", &["sh", "-c", "echo \"hi\"\nbye"], 12.5),
                entry(3, "日本語", &["日本語", "ü"], 100.0),
            ],
            SystemSnapshot::default(),
        );
        let rows = store.rows().iter().map(|r| view(r.key(), 0)).collect();
        (store, rows)
    }

    #[test]
    fn csv_quotes_per_rfc4180_and_prefixes_pid() {
        let (store, rows) = fixture();
        let cols = [ColumnId::Name, ColumnId::Cpu, ColumnId::Command];
        let out = render(
            &store,
            &rows,
            &cols,
            &ColumnCtx::default(),
            ExportFormat::Csv,
        );
        let expected = "PID,NAME,CPU%,COMMAND\r\n\
                        1,init,0.1,init\r\n\
                        2,\"we,ird\",12.5,\"sh -c echo \"\"hi\"\"\nbye\"\r\n\
                        3,日本語,100,日本語 ü\r\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn csv_keeps_column_order_without_duplicating_pid() {
        let (store, rows) = fixture();
        let cols = [ColumnId::User, ColumnId::Pid, ColumnId::User];
        let out = render(
            &store,
            &rows[..1],
            &cols,
            &ColumnCtx::default(),
            ExportFormat::Csv,
        );
        assert_eq!(out, "USER,PID\r\nroot,1\r\n");
    }

    #[test]
    fn csv_defuses_formulas() {
        let store = ProcessStore::from_entries(
            vec![
                entry(5, "=HYPERLINK(1)", &[], 0.0),
                entry(6, "-zsh", &[], 0.0),
            ],
            SystemSnapshot::default(),
        );
        let rows: Vec<ViewRow> = store.rows().iter().map(|r| view(r.key(), 0)).collect();
        let out = render(
            &store,
            &rows,
            &[ColumnId::Name],
            &ColumnCtx::default(),
            ExportFormat::Csv,
        );
        assert!(out.contains("\r\n5,'=HYPERLINK(1)\r\n"));
        assert!(out.contains("\r\n6,-zsh\r\n"));
    }

    #[test]
    fn empty_exports_are_well_formed() {
        let store = ProcessStore::new();
        let ctx = ColumnCtx::default();
        assert_eq!(
            render(&store, &[], &[ColumnId::Cpu], &ctx, ExportFormat::Csv),
            "PID,CPU%\r\n"
        );
        assert_eq!(render(&store, &[], &[], &ctx, ExportFormat::Csv), "PID\r\n");
        let json = render(&store, &[], &[ColumnId::Cpu], &ctx, ExportFormat::Json);
        assert_eq!(
            serde_json::from_str::<Value>(&json).unwrap(),
            Value::Array(vec![])
        );
        // Stale view rows are skipped rather than exported blank.
        let stale = [view(ProcessKey::new(42, 1), 0)];
        assert_eq!(
            render(&store, &stale, &[], &ctx, ExportFormat::Csv),
            "PID\r\n"
        );
    }

    #[test]
    fn json_types_and_keys() {
        let (store, rows) = fixture();
        let cols = [
            ColumnId::Name,
            ColumnId::Cpu,
            ColumnId::Rss,
            ColumnId::Ppid,
            ColumnId::Threads,
            ColumnId::Command,
        ];
        let out = render(
            &store,
            &rows,
            &cols,
            &ColumnCtx::default(),
            ExportFormat::Json,
        );
        let parsed: Value = serde_json::from_str(&out).expect("valid json");
        let items = parsed.as_array().unwrap();
        assert_eq!(items.len(), 3);
        let first = &items[0];
        assert_eq!(first["pid"], Value::from(1));
        assert_eq!(first["name"], Value::from("init"));
        assert_eq!(first["cpu"], serde_json::json!(0.1));
        assert_eq!(first["rss"], Value::from(2048));
        assert_eq!(first["ppid"], Value::Null);
        assert_eq!(first["threads"], Value::Null);
        assert!(first.get("depth").is_none());
        assert_eq!(items[1]["cmd"], Value::from("sh -c echo \"hi\"\nbye"));
        assert_eq!(items[1]["ppid"], Value::from(1));
        assert_eq!(items[2]["name"], Value::from("日本語"));
        assert_eq!(items[2]["cpu"], Value::from(100));
        // Keys follow column order.
        let line = out.lines().nth(1).unwrap();
        assert!(line.find("\"pid\"").unwrap() < line.find("\"name\"").unwrap());
        assert!(line.find("\"name\"").unwrap() < line.find("\"cmd\"").unwrap());
    }

    #[test]
    fn json_includes_depth_in_tree_mode() {
        let (store, _) = fixture();
        let keys: Vec<ProcessKey> = store.rows().iter().map(|r| r.key()).collect();
        let rows = vec![view(keys[0], 0), view(keys[1], 1), view(keys[2], 2)];
        let out = render(
            &store,
            &rows,
            &[ColumnId::Name],
            &ColumnCtx::default(),
            ExportFormat::Json,
        );
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let depths: Vec<u64> = parsed
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["depth"].as_u64().unwrap())
            .collect();
        assert_eq!(depths, vec![0, 1, 2]);
    }

    #[test]
    fn numbers_are_clean() {
        assert_eq!(number(3.0), Value::from(3));
        assert_eq!(number(0.1f32 as f64), serde_json::json!(0.1));
        assert_eq!(number(f64::NAN), Value::Null);
        assert_eq!(number(-5.0), Value::from(-5));
    }

    #[test]
    fn write_creates_file() {
        let dir = std::env::temp_dir().join(format!("bproc-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write("a,b\r\n", ExportFormat::Csv, Some(&dir)).unwrap();
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("csv"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a,b\r\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
