use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use c2pa_strip::{Findings, inspect, strip};
use clap::{Parser, Subcommand};

/// Detect and losslessly remove C2PA (Content Credentials) manifests from
/// JPEG, PNG and WebP images.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report C2PA content and generative-AI markers. Exits with 1 if any
    /// file carries some.
    Check {
        /// Image files or directories
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Descend into subdirectories
        #[arg(short, long)]
        recursive: bool,
    },
    /// Remove C2PA content, writing `<name>.clean.<ext>` next to each file.
    Remove {
        /// Image files or directories
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Descend into subdirectories
        #[arg(short, long)]
        recursive: bool,
        /// Output file (only with a single input file)
        #[arg(short, long, conflicts_with = "in_place")]
        output: Option<PathBuf>,
        /// Overwrite the input files
        #[arg(long)]
        in_place: bool,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(found) if found => ExitCode::from(1),
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

/// Returns whether `check` found C2PA content or AI markers anywhere.
fn run(cli: Cli) -> Result<bool> {
    match cli.command {
        Command::Check { paths, recursive } => {
            let (mut found, mut failed) = (false, false);
            for file in collect(&paths, recursive)? {
                match fs::read(&file)
                    .map_err(Into::into)
                    .and_then(|d| inspect(&d))
                {
                    Ok(f) if f.has_c2pa() || f.has_ai_markers() => {
                        found = true;
                        let tag = if f.has_c2pa() { "C2PA" } else { "AI" };
                        let mut what = describe(&f);
                        if f.has_ai_markers() {
                            if !what.is_empty() {
                                what.push_str(", ");
                            }
                            what.push_str(&f.ai_markers.join(", "));
                        }
                        println!("{tag:<7}{}  {what}", file.display());
                    }
                    Ok(_) => println!("clean  {}", file.display()),
                    Err(e) => {
                        failed = true;
                        eprintln!("skip   {}  ({e:#})", file.display());
                    }
                }
            }
            // A file we could not read must not pass as clean.
            if failed && !found {
                bail!("some files could not be checked");
            }
            Ok(found)
        }
        Command::Remove {
            paths,
            recursive,
            output,
            in_place,
        } => {
            let files = collect(&paths, recursive)?;
            if output.is_some() && files.len() != 1 {
                bail!("--output needs exactly one input file");
            }
            let mut failed = false;
            for file in &files {
                let dest = match (&output, in_place) {
                    (Some(out), _) => out.clone(),
                    (None, true) => file.clone(),
                    (None, false) => clean_path(file),
                };
                if let Err(e) = remove_one(file, &dest) {
                    failed = true;
                    eprintln!("skip   {}  ({e:#})", file.display());
                }
            }
            if failed {
                bail!("some files could not be processed");
            }
            Ok(false)
        }
    }
}

fn remove_one(file: &Path, dest: &Path) -> Result<()> {
    let data = fs::read(file)?;
    let (cleaned, removed) = strip(&data)?;
    if !removed.has_c2pa() {
        println!("clean  {}  (nothing to remove)", file.display());
        note_ai_markers(file, &removed);
        return Ok(());
    }
    // Never write a file that still carries C2PA content.
    let left = inspect(&cleaned)?;
    if left.has_c2pa() {
        bail!(
            "C2PA content still present after removal: {}",
            describe(&left)
        );
    }
    write_atomic(dest, &cleaned)?;
    println!(
        "fixed  {} -> {}  ({}; {} -> {} bytes)",
        file.display(),
        dest.display(),
        describe(&removed),
        data.len(),
        cleaned.len()
    );
    note_ai_markers(dest, &removed);
    Ok(())
}

/// AI markers outside C2PA are only detected, so say they are still there.
fn note_ai_markers(file: &Path, f: &Findings) {
    if f.has_ai_markers() {
        println!(
            "note   {}  AI marker(s) kept: {}",
            file.display(),
            f.ai_markers.join(", ")
        );
    }
}

fn describe(f: &Findings) -> String {
    let mut parts = Vec::new();
    if f.store_bytes > 0 {
        parts.push(format!(
            "manifest store {} bytes, {} manifest(s)",
            f.store_bytes,
            f.manifests.len()
        ));
    }
    if f.xmp_provenance {
        parts.push("XMP provenance link".into());
    }
    parts.join(", ")
}

/// `photo.jpg` -> `photo.clean.jpg`
fn clean_path(file: &Path) -> PathBuf {
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let name = match file.extension() {
        Some(ext) => format!("{stem}.clean.{}", ext.to_string_lossy()),
        None => format!("{stem}.clean"),
    };
    file.with_file_name(name)
}

/// Writes through a temporary file in the same directory, so an interrupted
/// `--in-place` run never leaves a truncated image behind.
fn write_atomic(dest: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, dest).with_context(|| format!("renaming to {}", dest.display()))
}

fn is_image(path: &Path) -> bool {
    let ext = path.extension().map(|e| e.to_ascii_lowercase());
    matches!(
        ext.as_ref().and_then(|e| e.to_str()),
        Some("jpg" | "jpeg" | "png" | "webp")
    )
}

/// Expands directories into the image files they contain. Files named on the
/// command line are taken as is, whatever their extension.
fn collect(paths: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            walk(path, recursive, &mut files)?;
        } else {
            files.push(path.clone());
        }
    }
    Ok(files)
}

fn walk(dir: &Path, recursive: bool, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                walk(&path, recursive, files)?;
            }
        } else if is_image(&path)
            && !path
                .file_stem()
                .is_some_and(|s| s.to_string_lossy().ends_with(".clean"))
        {
            files.push(path);
        }
    }
    Ok(())
}
