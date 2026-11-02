//! Running a prepared [`Job`] natively: stage its files in a fresh scratch folder, run the
//! `geolibre` program there, and read the new files back.

use std::path::{Path, PathBuf};

use rustgis_analysis::ToolResult;

use crate::{Job, finish, runner};

/// A scratch folder for one run, removed afterwards.
struct Scratch(PathBuf);

impl Scratch {
    /// A new, empty folder: never one that already exists (left over, or made by someone else
    /// on a shared machine), so no old file can pass for a tool's output.
    fn new() -> Result<Scratch, String> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let mut last = None;
        for _ in 0..16 {
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("rustgis-gp-{}-{nanos:x}-{n}", std::process::id()));
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(Scratch(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(format!("can't create a scratch folder: {e}")),
            }
        }
        Err(format!("can't create a scratch folder: {}", last.map(|e| e.to_string()).unwrap_or_default()))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The files in `dir` that aren't staged inputs, by file name.
fn new_files(dir: &Path, inputs: &[(String, Vec<u8>)]) -> Vec<(String, Vec<u8>)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if inputs.iter().any(|(f, _)| *f == name) {
                return None;
            }
            std::fs::read(e.path()).ok().map(|b| (name, b))
        })
        .collect()
}

/// Stages `files` in a scratch folder, `argv` pointing at it rather than `/work`, and runs the
/// tool there: the new files and what it printed. `finish` then turns them into layers, on the
/// session they were prepared for (the desktop app runs this on a thread of its own).
pub(crate) fn run_staged(
    tool: &str,
    argv: &[String],
    files: &[(String, Vec<u8>)],
) -> Result<(Vec<(String, Vec<u8>)>, Vec<String>), String> {
    let scratch = Scratch::new()?;
    for (name, bytes) in files {
        let path = scratch.0.join(name);
        std::fs::write(&path, bytes).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    }
    let dir = scratch.0.display().to_string();
    // `/work` is the working folder the browser's runner mounts: an argument naming a staged
    // file there names the scratch folder here.
    let argv: Vec<String> = argv.iter().map(|a| a.replace("/work/", &format!("{dir}/"))).collect();
    let lines = runner::run(tool, &argv, &scratch.0)?;
    Ok((new_files(&scratch.0, files), lines))
}

pub(crate) fn run(job: &Job) -> Result<ToolResult, String> {
    let scratch = Scratch::new()?;
    for (name, bytes) in &job.files {
        let path = scratch.0.join(name);
        std::fs::write(&path, bytes).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    }
    let lines = runner::run(&job.tool, &job.argv(&scratch.0.display().to_string()), &scratch.0)?;
    let files = new_files(&scratch.0, &job.files);
    Ok(finish(job, &files, &lines, Some(&scratch.0)))
}
