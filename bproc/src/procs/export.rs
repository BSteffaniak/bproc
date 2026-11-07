//! CSV / JSON export of the current table view.
use super::columns::{ColumnCtx, ColumnId};
use super::store::{ProcessStore, ViewRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
}

/// Render `rows` with `columns` in `format`.
pub fn render(
    store: &ProcessStore,
    rows: &[ViewRow],
    columns: &[ColumnId],
    ctx: &ColumnCtx,
    format: ExportFormat,
) -> String {
    let _ = (store, rows, columns, ctx, format);
    String::new()
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
