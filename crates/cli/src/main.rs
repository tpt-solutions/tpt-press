//! The `tpt-press` command-line tool.
//!
//! `pack`, `unpack` and `inspect` work on any file, using the core archive
//! format, with optional passphrase encryption. `pack-logs` and `query` work on
//! JSON Lines and CSV logs, which get a time index and a trained dictionary.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use tpt_press_assets as assets;
use tpt_press_core::archive::{self, PackOptions, UnpackOptions, DEFAULT_CHUNK_SIZE};
use tpt_press_core::codec::{DEFAULT_LEVEL, MAX_LEVEL, MIN_LEVEL};
use tpt_press_core::{open_archive, pack_encrypted, Error as CoreError};
use tpt_press_logs::{self as logs, Format, Query, DEFAULT_BLOCK_SIZE};
use tpt_press_tensors as tensors;

#[derive(Parser)]
#[command(
    name = "tpt-press",
    version,
    about = "Domain-aware compression and archiving"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compress a file into a tpt-press archive.
    Pack {
        /// File to compress.
        input: PathBuf,
        /// Archive to write. Removed if packing fails.
        output: PathBuf,
        /// Uncompressed bytes per chunk, up to 64 MiB.
        #[arg(long, default_value_t = DEFAULT_CHUNK_SIZE)]
        chunk_size: usize,
        /// zstd compression level.
        #[arg(long, default_value_t = DEFAULT_LEVEL, value_parser = clap::value_parser!(i32).range(i64::from(MIN_LEVEL)..=i64::from(MAX_LEVEL)))]
        level: i32,
        /// Encrypt the archive with a passphrase. Prompts for it unless
        /// --passphrase-file is given.
        #[arg(long)]
        encrypt: bool,
        /// Encrypt with the passphrase in this file. The trailing newline is
        /// ignored. Implies --encrypt.
        #[arg(long, value_name = "FILE")]
        passphrase_file: Option<PathBuf>,
    },
    /// Restore the original file from a tpt-press archive.
    Unpack {
        /// Archive to read.
        archive: PathBuf,
        /// File to write. Removed if unpacking fails.
        output: PathBuf,
        /// Passphrase file for an encrypted archive. Prompts if omitted.
        #[arg(long, value_name = "FILE")]
        passphrase_file: Option<PathBuf>,
    },
    /// Show an archive's structure and compression ratio, without extracting it.
    Inspect {
        /// Archive to read.
        archive: PathBuf,
        /// For tensor archives: also check every piece's checksum.
        #[arg(long)]
        verify: bool,
        /// Also print one row per chunk.
        #[arg(long)]
        chunks: bool,
        /// Passphrase file for an encrypted archive. Prompts if omitted.
        #[arg(long, value_name = "FILE")]
        passphrase_file: Option<PathBuf>,
    },
    /// Compress a JSON Lines or CSV log into a time-indexed archive.
    PackLogs {
        /// Log file to compress.
        input: PathBuf,
        /// Archive to write. Removed if packing fails.
        output: PathBuf,
        /// Record format. Inferred from the file extension if omitted: .csv is
        /// CSV, anything else is JSON Lines.
        #[arg(long, value_parser = ["jsonl", "csv"])]
        format: Option<String>,
        /// Field (JSON) or column (CSV) holding the RFC 3339 timestamp.
        #[arg(long, default_value = "timestamp")]
        timestamp_field: String,
        /// Uncompressed bytes per block before it is closed.
        #[arg(long, default_value_t = DEFAULT_BLOCK_SIZE)]
        block_size: usize,
        /// zstd compression level.
        #[arg(long, default_value_t = DEFAULT_LEVEL, value_parser = clap::value_parser!(i32).range(i64::from(MIN_LEVEL)..=i64::from(MAX_LEVEL)))]
        level: i32,
    },
    /// Pack a model into a tensor archive. Safetensors and GGUF are supported.
    PackTensors {
        /// Model file to pack: safetensors or GGUF, detected from the contents.
        input: PathBuf,
        /// Archive to write. Removed if packing fails.
        output: PathBuf,
        /// zstd compression level.
        #[arg(long, default_value_t = DEFAULT_LEVEL, value_parser = clap::value_parser!(i32).range(i64::from(MIN_LEVEL)..=i64::from(MAX_LEVEL)))]
        level: i32,
    },
    /// Write a tensor archive back out as a safetensors file.
    UnpackTensors {
        /// Tensor archive to read.
        archive: PathBuf,
        /// Safetensors file to write. Removed if unpacking fails.
        output: PathBuf,
    },
    /// Pack every file under a directory into an assets archive.
    PackAssets {
        /// Directory to pack. Names in the archive are relative paths inside it.
        input: PathBuf,
        /// Archive to write. Removed if packing fails.
        output: PathBuf,
        /// Store every asset raw, so readers can use it without copying.
        #[arg(long)]
        store: bool,
        /// zstd compression level for compressed assets.
        #[arg(long, default_value_t = DEFAULT_LEVEL, value_parser = clap::value_parser!(i32).range(i64::from(MIN_LEVEL)..=i64::from(MAX_LEVEL)))]
        level: i32,
    },
    /// Write every asset in an assets archive to a directory.
    UnpackAssets {
        /// Assets archive to read.
        archive: PathBuf,
        /// Directory to write into. Created if it does not exist.
        output_dir: PathBuf,
    },
    /// List the assets in an assets archive.
    ListAssets {
        /// Assets archive to read.
        archive: PathBuf,
        /// Also check every asset's checksum.
        #[arg(long)]
        verify: bool,
    },
    /// Print the records of a logs archive that match a time range and columns.
    ///
    /// Records go to stdout, and the summary goes to stderr, so the output
    /// can be piped.
    Query {
        /// Logs archive to read.
        archive: PathBuf,
        /// Earliest timestamp to include, as RFC 3339.
        #[arg(long)]
        from: Option<String>,
        /// Latest timestamp to include, as RFC 3339.
        #[arg(long)]
        to: Option<String>,
        /// Comma-separated columns to output, in order. Outputs whole records if omitted.
        #[arg(long, value_delimiter = ',')]
        columns: Option<Vec<String>>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Pack {
            input,
            output,
            chunk_size,
            level,
            encrypt,
            passphrase_file,
        } => pack(
            &input,
            &output,
            chunk_size,
            level,
            encrypt,
            passphrase_file.as_deref(),
        ),
        Command::Unpack {
            archive,
            output,
            passphrase_file,
        } => unpack(&archive, &output, passphrase_file.as_deref()),
        Command::Inspect {
            archive,
            chunks,
            passphrase_file,
            verify,
        } => inspect(&archive, chunks, passphrase_file.as_deref(), verify),
        Command::PackTensors {
            input,
            output,
            level,
        } => pack_tensors(&input, &output, level),
        Command::UnpackTensors { archive, output } => unpack_tensors(&archive, &output),
        Command::PackLogs {
            input,
            output,
            format,
            timestamp_field,
            block_size,
            level,
        } => pack_logs(
            &input,
            &output,
            format.as_deref(),
            &timestamp_field,
            block_size,
            level,
        ),
        Command::Query {
            archive,
            from,
            to,
            columns,
        } => query_logs(&archive, from.as_deref(), to.as_deref(), columns),
        Command::PackAssets {
            input,
            output,
            store,
            level,
        } => pack_assets(&input, &output, store, level),
        Command::UnpackAssets {
            archive,
            output_dir,
        } => unpack_assets(&archive, &output_dir),
        Command::ListAssets { archive, verify } => list_assets(&archive, verify),
    };

    match result {
        Ok(summary) if summary.is_empty() => ExitCode::SUCCESS,
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("tpt-press: {e}");
            ExitCode::FAILURE
        }
    }
}

fn pack(
    input: &Path,
    output: &Path,
    chunk_size: usize,
    level: i32,
    encrypt: bool,
    passphrase_file: Option<&Path>,
) -> Result<String, Box<dyn Error>> {
    let opts = PackOptions { chunk_size, level };
    let reader = open_file(input, "packing")?;

    let passphrase = match passphrase_file {
        Some(file) => Some(read_passphrase_file(file)?),
        None if encrypt => Some(prompt_new_passphrase()?),
        None => None,
    };
    let writer = BufWriter::new(create(output)?);

    let stats = finish_or_remove(output, || match &passphrase {
        Some(p) => pack_encrypted(reader, writer, &opts, p),
        None => archive::pack(reader, writer, &opts),
    })?;
    let ratio = stats.uncompressed_bytes as f64 / stats.archive_bytes.max(1) as f64;
    let encrypted = if passphrase.is_some() {
        ", encrypted"
    } else {
        ""
    };
    Ok(format!(
        "packed {} -> {} bytes ({ratio:.2}x, {} chunks{encrypted})",
        stats.uncompressed_bytes, stats.archive_bytes, stats.chunks
    ))
}

fn unpack(
    archive_path: &Path,
    output: &Path,
    passphrase_file: Option<&Path>,
) -> Result<String, Box<dyn Error>> {
    let reader = open_archive_file(archive_path, passphrase_file, "unpacking")?;
    let writer = BufWriter::new(create(output)?);

    let stats = finish_or_remove(output, || {
        archive::unpack(reader, writer, &UnpackOptions::default())
    })?;
    Ok(format!(
        "unpacked {} bytes from {} chunks",
        stats.uncompressed_bytes, stats.chunks
    ))
}

fn inspect(
    path: &Path,
    show_chunks: bool,
    passphrase_file: Option<&Path>,
    verify: bool,
) -> Result<String, Box<dyn Error>> {
    match file_magic(path)?.as_slice() {
        b"TPLG" => {
            if show_chunks {
                return Err("--chunks applies to generic archives, not logs archives".into());
            }
            return inspect_logs(path);
        }
        b"TPTN" => return inspect_tensors(path, verify),
        _ => {}
    }

    let reader = open_archive_file(path, passphrase_file, "reading")?;
    let inspection = archive::inspect(reader)?;
    let stats = &inspection.stats;

    let mut lines = vec![
        format!("archive:       {}", path.display()),
        format!("chunks:        {}", stats.chunks),
        format!("uncompressed:  {} bytes", stats.uncompressed_bytes),
        format!("archive size:  {} bytes", stats.archive_bytes),
        format!("ratio:         {:.2}x", inspection.ratio()),
    ];

    if show_chunks {
        lines.push(String::new());
        lines.push(format!(
            "{:>6} {:>12} {:>12} {:>14} {:>7}",
            "chunk", "offset", "compressed", "uncompressed", "ratio"
        ));
        for (i, chunk) in inspection.chunks.iter().enumerate() {
            let ratio = f64::from(chunk.uncompressed_len) / f64::from(chunk.compressed_len.max(1));
            lines.push(format!(
                "{i:>6} {:>12} {:>12} {:>14} {ratio:>6.2}x",
                chunk.offset, chunk.compressed_len, chunk.uncompressed_len
            ));
        }
    }

    Ok(lines.join("\n"))
}

fn pack_logs(
    input: &Path,
    output: &Path,
    format: Option<&str>,
    timestamp_field: &str,
    block_size: usize,
    level: i32,
) -> Result<String, Box<dyn Error>> {
    let format = match format {
        Some("csv") => Format::Csv,
        Some(_) => Format::Jsonl,
        None if input
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("csv")) =>
        {
            Format::Csv
        }
        None => Format::Jsonl,
    };
    let opts = logs::PackOptions {
        format,
        timestamp_field: timestamp_field.to_owned(),
        block_size,
        level,
    };

    let reader = open_file(input, "packing")?;
    let writer = BufWriter::new(create(output)?);
    let stats = finish_or_remove(output, || logs::pack(reader, writer, &opts))?;

    let ratio = stats.input_bytes as f64 / stats.archive_bytes.max(1) as f64;
    Ok(format!(
        "packed {} records in {} blocks: {} -> {} bytes ({ratio:.2}x, {}-byte dictionary)",
        stats.records, stats.blocks, stats.input_bytes, stats.archive_bytes, stats.dictionary_bytes
    ))
}

/// Writes matching records to stdout and the summary to stderr. Returns an
/// empty string so `main` prints nothing more to stdout.
fn pack_assets(
    input: &Path,
    output: &Path,
    store: bool,
    level: i32,
) -> Result<String, Box<dyn Error>> {
    let mut files = Vec::new();
    collect_files(input, &mut files)?;
    files.sort();

    let opts = assets::PackOptions {
        level,
        store_all: store,
    };
    // Read each file only when it is packed, so memory holds one file at a time.
    let items = files.into_iter().map(|(path, name)| {
        let contents = fs::read(&path).map_err(|e| {
            assets::Error::Io(io::Error::new(
                e.kind(),
                format!("cannot read {}: {e}", path.display()),
            ))
        })?;
        Ok((name, contents))
    });

    let writer = BufWriter::new(create(output)?);
    let stats = finish_or_remove(output, || assets::pack(items, writer, &opts))?;
    Ok(format!(
        "packed {} assets ({} stored, {} compressed): {} -> {} bytes",
        stats.assets, stats.stored, stats.compressed, stats.input_bytes, stats.archive_bytes
    ))
}

/// Collects regular files under `dir`, with their archive names. Names use `/`
/// separators and are relative to `dir`.
fn collect_files(dir: &Path, out: &mut Vec<(PathBuf, String)>) -> Result<(), Box<dyn Error>> {
    let root = dir.to_path_buf();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)
            .map_err(|e| format!("cannot read directory {}: {e}", current.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                let relative = path.strip_prefix(&root)?;
                let parts: Result<Vec<&str>, _> = relative
                    .components()
                    .map(|c| {
                        c.as_os_str()
                            .to_str()
                            .ok_or_else(|| format!("non-UTF-8 file name: {}", path.display()))
                    })
                    .collect();
                out.push((path.clone(), parts?.join("/")));
            }
        }
    }
    Ok(())
}

fn unpack_assets(archive_path: &Path, output_dir: &Path) -> Result<String, Box<dyn Error>> {
    let archive = assets::Archive::open(archive_path)?;
    fs::create_dir_all(output_dir)
        .map_err(|e| format!("cannot create {}: {e}", output_dir.display()))?;

    let mut count = 0usize;
    for entry in archive.entries() {
        // Names were checked when the archive was opened, so each one stays inside `output_dir`.
        let target = entry
            .name
            .split('/')
            .fold(output_dir.to_path_buf(), |p, part| p.join(part));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let asset = archive
            .get(&entry.name)?
            .ok_or("asset listed in the index is missing")?;
        fs::write(&target, &*asset)
            .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        count += 1;
    }
    Ok(format!(
        "unpacked {count} assets to {}",
        output_dir.display()
    ))
}

fn pack_tensors(input: &Path, output: &Path, level: i32) -> Result<String, Box<dyn Error>> {
    let reader = open_file(input, "packing")?;
    let writer = BufWriter::new(create(output)?);
    let opts = tensors::PackOptions { level };
    let stats = finish_or_remove(output, || tensors::pack(reader, writer, &opts))?;
    let ratio = stats.input_bytes as f64 / stats.archive_bytes.max(1) as f64;
    Ok(format!(
        "packed {} tensors in {} pieces ({} compressed): {} -> {} bytes ({ratio:.2}x)",
        stats.tensors,
        stats.pieces,
        stats.compressed_pieces,
        stats.input_bytes,
        stats.archive_bytes
    ))
}

fn unpack_tensors(archive_path: &Path, output: &Path) -> Result<String, Box<dyn Error>> {
    let mut archive = tensors::Archive::open(open_file(archive_path, "unpacking")?)?;
    let writer = BufWriter::new(create(output)?);
    let stats = finish_or_remove(output, || tensors::unpack(&mut archive, writer))?;
    Ok(format!(
        "unpacked {} tensors, {} bytes of data, to a safetensors file",
        stats.tensors, stats.data_bytes
    ))
}

fn inspect_tensors(path: &Path, verify: bool) -> Result<String, Box<dyn Error>> {
    let file_bytes = fs::metadata(path)?.len();
    let mut archive = tensors::Archive::open(open_file(path, "reading")?)?;

    let mut dtypes: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut parameters = 0u64;
    let mut data_bytes = 0u64;
    for t in archive.tensors() {
        let count = t
            .shape
            .iter()
            .try_fold(1u64, |acc, &d| acc.checked_mul(d))
            .ok_or("tensor element count overflows")?;
        parameters = parameters.saturating_add(count);
        data_bytes += t.byte_len;
        let entry = dtypes.entry(t.dtype.clone()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += t.byte_len;
    }

    let mut lines = vec![
        format!("archive:        {}", path.display()),
        format!("tensors:        {}", archive.tensors().len()),
        format!("parameters:     {parameters}"),
    ];
    for (dtype, (count, bytes)) in &dtypes {
        lines.push(format!("  {dtype:<10} {count} tensors, {bytes} bytes"));
    }
    if !archive.metadata().is_empty() {
        lines.push("metadata:".to_owned());
        for (key, value) in archive.metadata() {
            lines.push(format!("  {key} = {value}"));
        }
    }
    lines.push(format!("data:           {data_bytes} bytes"));
    lines.push(format!("archive size:   {file_bytes} bytes"));
    if verify {
        archive.verify()?;
        lines.push("all checksums match".to_owned());
    }
    Ok(lines.join("\n"))
}

fn list_assets(archive_path: &Path, verify: bool) -> Result<String, Box<dyn Error>> {
    let archive = assets::Archive::open(archive_path)?;
    let mut lines = Vec::with_capacity(archive.entries().len() + 2);
    lines.push(format!(
        "{:>12} {:>12}  {:<8}  name",
        "size", "stored", "storage"
    ));
    for entry in archive.entries() {
        let storage = match entry.compression {
            assets::Compression::Stored => "raw",
            assets::Compression::Zstd => "zstd",
        };
        lines.push(format!(
            "{:>12} {:>12}  {:<8}  {}",
            entry.raw_len, entry.stored_len, storage, entry.name
        ));
    }
    if verify {
        archive.verify()?;
        lines.push(format!("all {} checksums match", archive.entries().len()));
    }
    Ok(lines.join("\n"))
}

fn query_logs(
    archive_path: &Path,
    from: Option<&str>,
    to: Option<&str>,
    columns: Option<Vec<String>>,
) -> Result<String, Box<dyn Error>> {
    let query = Query {
        from: from.map(logs::parse_timestamp).transpose()?,
        to: to.map(logs::parse_timestamp).transpose()?,
        columns,
    };

    let reader = open_file(archive_path, "reading")?;
    let stdout = io::stdout();
    // A large buffer means fewer writes to the pipe, which is much faster when
    // the output is piped into another program.
    let writer = BufWriter::with_capacity(1024 * 1024, stdout.lock());
    let stats = match logs::query(reader, writer, &query) {
        Ok(stats) => stats,
        // The reader stopped early, as with `query ... | head`. That is not an error.
        Err(logs::Error::Io(e)) if e.kind() == io::ErrorKind::BrokenPipe => {
            return Ok(String::new())
        }
        Err(e) => return Err(e.into()),
    };

    eprintln!(
        "matched {} records; read {} of {} blocks; decompressed {} columns",
        stats.records_matched, stats.blocks_read, stats.blocks_total, stats.columns_read
    );
    Ok(String::new())
}

/// Whether the file starts with the logs archive magic. Logs archives are not encrypted.
/// Reads the first four bytes of a file, which identify its archive type.
fn file_magic(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let file = open_file(path, "reading")?;
    let mut magic = Vec::new();
    file.take(4).read_to_end(&mut magic)?;
    Ok(magic)
}

fn inspect_logs(path: &Path) -> Result<String, Box<dyn Error>> {
    let summary = logs::inspect(open_file(path, "reading")?)?;
    let range = match summary.time_range {
        Some((from, to)) => format!(
            "{} to {}",
            logs::format_timestamp(from),
            logs::format_timestamp(to)
        ),
        None => "none".to_owned(),
    };
    let format = match summary.format {
        Format::Jsonl => "jsonl",
        Format::Csv => "csv",
    };

    Ok([
        format!("archive:        {}", path.display()),
        format!("format:         {format}"),
        format!("records:        {}", summary.records),
        format!(
            "blocks:         {} ({} with records lacking a timestamp)",
            summary.blocks, summary.untimed_blocks
        ),
        format!("columns:        {}", summary.columns.join(", ")),
        format!("time range:     {range}"),
        format!("dictionary:     {} bytes", summary.dictionary_bytes),
        format!("column data:    {} bytes", summary.uncompressed_bytes),
        format!("archive size:   {} bytes", summary.archive_bytes),
        format!("ratio:          {:.2}x", summary.ratio()),
    ]
    .join("\n"))
}

/// Opens an archive, decrypting it if needed. An encrypted archive uses
/// `passphrase_file` if given, otherwise the user is prompted.
fn open_archive_file(
    path: &Path,
    passphrase_file: Option<&Path>,
    label: &'static str,
) -> Result<Box<dyn Read>, Box<dyn Error>> {
    if let Some(file) = passphrase_file {
        let passphrase = read_passphrase_file(file)?;
        return Ok(open_archive(open_file(path, label)?, Some(&passphrase))?);
    }

    match open_archive(open_file(path, label)?, None) {
        Ok(reader) => Ok(reader),
        Err(CoreError::PassphraseRequired) => {
            let passphrase = prompt("Passphrase: ")?;
            Ok(open_archive(open_file(path, label)?, Some(&passphrase))?)
        }
        Err(e) => Err(e.into()),
    }
}

fn read_passphrase_file(path: &Path) -> Result<String, Box<dyn Error>> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("cannot read passphrase file {}: {e}", path.display()))?;
    let passphrase = text.trim_end_matches(['\r', '\n']).to_owned();
    if passphrase.is_empty() {
        return Err("passphrase file is empty".into());
    }
    Ok(passphrase)
}

fn prompt(message: &str) -> Result<String, Box<dyn Error>> {
    let passphrase =
        rpassword::prompt_password(message).map_err(|e| format!("cannot read passphrase: {e}"))?;
    if passphrase.is_empty() {
        return Err("passphrase must not be empty".into());
    }
    Ok(passphrase)
}

fn prompt_new_passphrase() -> Result<String, Box<dyn Error>> {
    let first = prompt("New passphrase: ")?;
    let second = prompt("Repeat passphrase: ")?;
    if first != second {
        return Err("passphrases do not match".into());
    }
    Ok(first)
}

/// Opens a file for reading, with a progress bar that tracks bytes read.
fn open_file(
    path: &Path,
    label: &'static str,
) -> Result<BufReader<Progress<File>>, Box<dyn Error>> {
    let total = file_len(path)?;
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    Ok(BufReader::new(Progress::new(file, total, label)))
}

fn create(path: &Path) -> Result<File, Box<dyn Error>> {
    File::create(path).map_err(|e| format!("cannot create {}: {e}", path.display()).into())
}

fn file_len(path: &Path) -> Result<u64, Box<dyn Error>> {
    let meta = fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(meta.len())
}

/// Runs `op`, deleting `output` if it fails so no partial file is left behind.
fn finish_or_remove<T, E: Error + 'static>(
    output: &Path,
    op: impl FnOnce() -> Result<T, E>,
) -> Result<T, Box<dyn Error>> {
    op().map_err(|e| {
        let _ = fs::remove_file(output);
        Box::new(e) as Box<dyn Error>
    })
}

/// Reader wrapper that advances a progress bar by the bytes it returns.
struct Progress<R> {
    inner: R,
    bar: ProgressBar,
}

impl<R> Progress<R> {
    fn new(inner: R, total: u64, label: &'static str) -> Self {
        let style = ProgressStyle::with_template(
            "{msg} [{bar:40}] {bytes}/{total_bytes} {bytes_per_sec} ETA {eta}",
        )
        .unwrap_or_else(|_| ProgressStyle::default_bar());
        let bar = ProgressBar::new(total)
            .with_style(style)
            .with_message(label);
        Self { inner, bar }
    }
}

impl<R: Read> Read for Progress<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bar.inc(n as u64);
        Ok(n)
    }
}

impl<R: Seek> Seek for Progress<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let at = self.inner.seek(pos)?;
        self.bar.set_position(at);
        Ok(at)
    }
}

impl<R> Drop for Progress<R> {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}
