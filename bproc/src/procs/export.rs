//! CSV / JSON export of the current table view.
use super::columns::{ColumnCtx, ColumnId};
use super::store::{ProcessStore, Row, ViewRow};
use serde_json::Value;
use std::path::{Path, PathBuf};

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
/// and command lines are attacker controlled). A leading `-` is defused unless
/// a letter or another `-` follows, so login shells (`-zsh`) and flags
/// (`--verbose`) stay readable while `-1+cmd|…` and `-=…` do not run.
fn defuse(text: String) -> String {
    let mut chars = text.chars();
    let dangerous = match chars.next() {
        Some('=' | '+' | '@' | '\t' | '\r') => true,
        Some('-') => !chars.next().is_some_and(|c| c.is_alphabetic() || c == '-'),
        _ => false,
    };
    if dangerous { format!("'{text}") } else { text }
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

/// Most numbered variants tried when exports collide within one second.
const MAX_SUFFIX: u32 = 100;

/// Write an export to `dir` and return its absolute path.
///
/// Without `dir` the file goes to the current directory, or, when running as
/// root, to the invoking account's downloads/home directory so `sudo bproc`
/// in a shared directory (`/tmp`) does not write where others can plant
/// files. The file is always created fresh (`O_EXCL`): an existing file or
/// symlink is never followed or overwritten; a `-1`, `-2`, … suffix is added
/// instead.
pub fn write(contents: &str, format: ExportFormat, dir: Option<&Path>) -> std::io::Result<PathBuf> {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => default_dir(),
    };
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let ext = match format {
        ExportFormat::Csv => "csv",
        ExportFormat::Json => "json",
    };
    write_new(&dir, &stamp, ext, contents)
}

fn write_new(dir: &Path, stamp: &str, ext: &str, contents: &str) -> std::io::Result<PathBuf> {
    use std::io::Write as _;
    for n in 0..=MAX_SUFFIX {
        let name = if n == 0 {
            format!("bproc-{stamp}.{ext}")
        } else {
            format!("bproc-{stamp}-{n}.{ext}")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("too many exports named bproc-{stamp}-*.{ext}"),
    ))
}

fn default_dir() -> PathBuf {
    let cwd = || std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if running_as_root() {
        dirs::download_dir()
            .filter(|d| d.is_dir())
            .or_else(dirs::home_dir)
            .unwrap_or_else(cwd)
    } else {
        cwd()
    }
}

#[cfg(unix)]
fn running_as_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn running_as_root() -> bool {
    false
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
    fn defuse_leading_minus_unless_word_or_flag() {
        let d = |s: &str| defuse(s.to_string());
        assert_eq!(d("-1+cmd|' /C calc'!A0"), "'-1+cmd|' /C calc'!A0");
        assert_eq!(d("-=1"), "'-=1");
        assert_eq!(d("-@SUM(1)"), "'-@SUM(1)");
        assert_eq!(d("-"), "'-");
        assert_eq!(d("-zsh"), "-zsh");
        assert_eq!(d("--flag"), "--flag");
        assert_eq!(d("+1"), "'+1");
        assert_eq!(d("@x"), "'@x");
        assert_eq!(d("\t1"), "'\t1");
        assert_eq!(d("node"), "node");
        assert_eq!(d(""), "");
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

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bproc-export-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_creates_file() {
        let dir = scratch_dir("create");
        let path = write("a,b\r\n", ExportFormat::Csv, Some(&dir)).unwrap();
        assert!(path.is_absolute());
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("csv"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a,b\r\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_never_overwrites_same_second_exports() {
        let dir = scratch_dir("collide");
        let a = write_new(&dir, "20260101-000000", "csv", "first").unwrap();
        let b = write_new(&dir, "20260101-000000", "csv", "second").unwrap();
        let c = write_new(&dir, "20260101-000000", "csv", "third").unwrap();
        assert_eq!(a.file_name().unwrap(), "bproc-20260101-000000.csv");
        assert_eq!(b.file_name().unwrap(), "bproc-20260101-000000-1.csv");
        assert_eq!(c.file_name().unwrap(), "bproc-20260101-000000-2.csv");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_does_not_follow_planted_symlinks() {
        let dir = scratch_dir("symlink");
        let victim = dir.join("victim");
        std::fs::write(&victim, "precious").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("bproc-20260101-000000.json")).unwrap();
        let path = write_new(&dir, "20260101-000000", "json", "[]").unwrap();
        assert_eq!(path.file_name().unwrap(), "bproc-20260101-000000-1.json");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
