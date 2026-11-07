//! Where interactive exports land: a known directory (Downloads, else home,
//! else the working directory), never overwriting an earlier export, reported
//! as an absolute path.
use crate::procs::export::ExportFormat;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Directory for interactive exports.
pub fn default_dir() -> PathBuf {
    [
        crate::config::invoking_download_dir(),
        crate::config::invoking_home_dir(),
    ]
    .into_iter()
    .flatten()
    .find(|d| d.is_dir())
    .or_else(|| std::env::current_dir().ok())
    .unwrap_or_else(|| PathBuf::from("."))
}

fn extension(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Csv => "csv",
        ExportFormat::Json => "json",
    }
}

/// Write `contents` to a new file in `dir`; a name taken in the same second
/// gets a `-1`, `-2`, … suffix instead of being overwritten.
pub fn write_in(dir: &Path, contents: &str, format: ExportFormat) -> std::io::Result<PathBuf> {
    let stem = format!("bproc-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let ext = extension(format);
    for n in 0..1000 {
        let name = if n == 0 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())?;
                crate::config::chown_to_invoker(&path);
                return Ok(std::fs::canonicalize(&path).unwrap_or(path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "too many exports with the same timestamp",
    ))
}

/// Write an interactive export to [`default_dir`].
pub fn write(contents: &str, format: ExportFormat) -> std::io::Result<PathBuf> {
    write_in(&default_dir(), contents, format)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_second_exports_never_overwrite_and_paths_are_absolute() {
        let dir = std::env::temp_dir().join(format!("bproc-export-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = write_in(&dir, "a", ExportFormat::Csv).unwrap();
        let second = write_in(&dir, "b", ExportFormat::Csv).unwrap();
        assert_ne!(first, second);
        assert!(first.is_absolute());
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "b");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
