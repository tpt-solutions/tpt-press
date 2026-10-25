//! The `tpt-press` command-line tool.
//!
//! `pack`, `unpack` and `inspect` work on any file, using the core archive
//! format, with optional passphrase encryption. `pack-logs` and `query` work on
//! JSON Lines and CSV logs, which get a time index and a trained dictionary.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use age::secrecy::ExposeSecret;
use age::x25519;
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use std::str::FromStr;
use tpt_press_arrays::{self as arrays, ArraysArchive};
use tpt_press_assets::{self as assets, AssetsArchive};
use tpt_press_core::archive::{self, PackOptions, UnpackOptions, DEFAULT_CHUNK_SIZE};
use tpt_press_core::codec::{DEFAULT_LEVEL, MAX_LEVEL, MIN_LEVEL};
use tpt_press_core::ArchiveFormat;
use tpt_press_core::Chunking;
use tpt_press_core::{
    open_archive, open_with_identities, pack_encrypted, pack_for_recipients, Error as CoreError,
};
use tpt_press_datasets as datasets;
use tpt_press_genomics::{self as genomics, GenomicsArchive};
use tpt_press_logs::{self as logs, Format, LogsArchive, Query, DEFAULT_BLOCK_SIZE};
use tpt_press_tensors::{self as tensors, TensorsArchive};

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
        /// Uncompressed bytes per chunk, up to 64 MiB. With --content-defined,
        /// the average, a power of two from 256 bytes to 16 MiB.
        #[arg(long, default_value_t = DEFAULT_CHUNK_SIZE)]
        chunk_size: usize,
        /// Split at content-defined boundaries, not fixed offsets. An edit then
        /// changes only nearby chunks, which helps deduplication across versions.
        #[arg(long)]
        content_defined: bool,
        /// Train a zstd dictionary of at most this many bytes (up to 1 MiB) from
        /// the start of the input, and compress with it. Pays off on large inputs
        /// with repeated structure, such as logs.
        #[arg(long, value_name = "BYTES")]
        dictionary: Option<usize>,
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
        /// Encrypt to this age public key (`age1...`). Repeat for more recipients.
        /// Any of them can open the archive with its identity. Cannot be combined
        /// with a passphrase.
        #[arg(
            long,
            value_name = "KEY",
            conflicts_with_all = ["encrypt", "passphrase_file"]
        )]
        recipient: Vec<String>,
        /// Encrypt to the public keys in this file, one per line. Lines starting
        /// with `#` are ignored.
        #[arg(
            long,
            value_name = "FILE",
            conflicts_with_all = ["encrypt", "passphrase_file"]
        )]
        recipient_file: Option<PathBuf>,
    },
    /// Create an age identity file, and print its public key to give to senders.
    Keygen {
        /// Identity file to write. Keep it private: it can open any archive
        /// encrypted to its public key.
        output: PathBuf,
    },
    /// Restore the original file from a tpt-press archive.
    Unpack {
        /// Archive to read.
        archive: PathBuf,
        /// File to write. Removed if unpacking fails.
        output: PathBuf,
        /// Passphrase file for an encrypted archive. Prompts if omitted.
        #[arg(long, value_name = "FILE", conflicts_with = "identity")]
        passphrase_file: Option<PathBuf>,
        /// age identity file for an archive encrypted to public keys.
        #[arg(long, value_name = "FILE")]
        identity: Option<PathBuf>,
        /// Refuse to write more than this many bytes. Use it for archives from
        /// untrusted sources, which could unpack to far more than their size.
        #[arg(long, value_name = "BYTES")]
        max_output: Option<u64>,
    },
    /// Show an archive's structure and compression ratio, without extracting it.
    Inspect {
        /// Archive to read.
        archive: PathBuf,
        /// For tensor and assets archives: also check every checksum.
        #[arg(long)]
        verify: bool,
        /// Also print one row per chunk.
        #[arg(long)]
        chunks: bool,
        /// Tensor archives only: also list every tensor with its dtype, shape and size.
        #[arg(long)]
        tensors: bool,
        /// Passphrase file for an encrypted archive. Prompts if omitted.
        #[arg(long, value_name = "FILE", conflicts_with = "identity")]
        passphrase_file: Option<PathBuf>,
        /// age identity file for an archive encrypted to public keys.
        #[arg(long, value_name = "FILE")]
        identity: Option<PathBuf>,
    },
    /// Compress a JSON Lines or CSV log into a time-indexed archive.
    PackLogs {
        /// Log file to compress.
        input: PathBuf,
        /// Archive to write. Removed if packing fails.
        output: PathBuf,
        /// Record format. Inferred from the file extension if omitted: .csv is
        /// CSV, .prom is Prometheus text, anything else is JSON Lines.
        #[arg(long, value_parser = ["jsonl", "csv", "prometheus"])]
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
        /// Do not train a dictionary. Each block then compresses alone.
        #[arg(long)]
        no_dictionary: bool,
        /// Encrypt with a passphrase, prompted for unless --passphrase-file is given.
        #[arg(long, conflicts_with = "recipient", conflicts_with = "recipient_file")]
        encrypt: bool,
        /// Encrypt with the passphrase in this file. Implies --encrypt.
        #[arg(
            long,
            value_name = "FILE",
            conflicts_with = "recipient",
            conflicts_with = "recipient_file"
        )]
        passphrase_file: Option<PathBuf>,
        /// Encrypt to this age public key (`age1...`). Repeat for more recipients.
        #[arg(
            long,
            value_name = "KEY",
            conflicts_with = "encrypt",
            conflicts_with = "passphrase_file"
        )]
        recipient: Vec<String>,
        /// Encrypt to the public keys in this file, one per line.
        #[arg(
            long,
            value_name = "FILE",
            conflicts_with = "encrypt",
            conflicts_with = "passphrase_file"
        )]
        recipient_file: Option<PathBuf>,
    },
    /// Pack a model into a tensor archive. Safetensors, GGUF, ONNX and PyTorch checkpoints are supported.
    PackTensors {
        /// Model file to pack, detected from its contents.
        input: PathBuf,
        /// Archive to write. Removed if packing fails. With --shard-size, a
        /// directory to create, holding the shards and a manifest.
        output: PathBuf,
        /// zstd compression level.
        #[arg(long, default_value_t = DEFAULT_LEVEL, value_parser = clap::value_parser!(i32).range(i64::from(MIN_LEVEL)..=i64::from(MAX_LEVEL)))]
        level: i32,
        /// Split the model into shards of at most this many bytes of tensor data,
        /// written into the output directory. A larger tensor gets a shard to itself.
        #[arg(long, value_name = "BYTES")]
        shard_size: Option<u64>,
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
    /// Convert a WebDataset tar (files named `<key>.<member>`) into a dataset archive.
    ImportDataset {
        /// Tar file to read.
        input: PathBuf,
        /// Dataset archive to write. Removed if importing fails.
        output: PathBuf,
        /// zstd compression level for members that are not compressed media.
        #[arg(long, default_value_t = DEFAULT_LEVEL, value_parser = clap::value_parser!(i32).range(i64::from(MIN_LEVEL)..=i64::from(MAX_LEVEL)))]
        level: i32,
    },
    /// List the first samples of a dataset archive.
    ListDataset {
        /// Dataset archive to read.
        archive: PathBuf,
        /// Also check every member's checksum.
        #[arg(long)]
        verify: bool,
    },
    /// Print the records of a logs archive that match a time range and columns.
    ///
    /// Records go to stdout, and the summary goes to stderr, so the output
    /// can be piped.
    Query {
        /// Logs or tensor archive to read.
        archive: PathBuf,
        /// Tensor archives only: the tensor to output, as raw bytes.
        name: Option<String>,
        /// Earliest timestamp to include, as RFC 3339. Logs only.
        #[arg(long)]
        from: Option<String>,
        /// Latest timestamp to include, as RFC 3339. Logs only.
        #[arg(long)]
        to: Option<String>,
        /// Comma-separated columns to output, in order. Outputs whole records if omitted. Logs only.
        #[arg(long, value_delimiter = ',')]
        columns: Option<Vec<String>>,
        /// Logs only, in builds with the `strata` feature: run this SQL over the
        /// matching records and print the result. The table is named `logs`.
        #[arg(long, value_name = "SQL")]
        sql: Option<String>,
        /// Tensor archives only: write the tensor to this file, not stdout.
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Encrypted logs archives: passphrase file. Prompts if omitted.
        #[arg(long, value_name = "FILE", conflicts_with = "identity")]
        passphrase_file: Option<PathBuf>,
        /// Encrypted logs archives: age identity file.
        #[arg(long, value_name = "FILE")]
        identity: Option<PathBuf>,
    },
    /// Compress a FASTA or FASTQ file into a genomics archive. The format is
    /// taken from the first byte: `>` is FASTA, `@` is FASTQ.
    PackGenomics {
        /// FASTA or FASTQ file to compress.
        input: PathBuf,
        /// Archive to write. Removed if packing fails.
        output: PathBuf,
        /// Sequence bases per block before compression. Smaller blocks make
        /// `fetch-genomics` read less, and compress a little worse.
        #[arg(long, default_value_t = genomics::DEFAULT_BLOCK_BASES)]
        block_bases: usize,
        /// zstd compression level.
        #[arg(long, default_value_t = DEFAULT_LEVEL)]
        level: i32,
    },
    /// Write the original FASTA or FASTQ back out, byte for byte.
    UnpackGenomics {
        /// Genomics archive to read.
        archive: PathBuf,
        /// File to write.
        output: PathBuf,
    },
    /// Print one record from a genomics archive, or part of it, by name.
    ///
    /// Only the block that holds the record is decoded. The record goes to
    /// stdout as FASTA or FASTQ, in the archive's own format.
    FetchGenomics {
        /// Genomics archive to read.
        archive: PathBuf,
        /// Record name: the header up to its first whitespace.
        id: String,
        /// First base of the range, 1-based. Needs `--end`.
        #[arg(long, requires = "end")]
        start: Option<u64>,
        /// Last base of the range, 1-based and inclusive. Needs `--start`.
        #[arg(long, requires = "start")]
        end: Option<u64>,
    },
    /// Compress an array into a chunked arrays archive. A `.npy` file keeps its
    /// header, so unpacking reproduces it. Raw bytes need `--dtype` and `--shape`.
    PackArrays {
        /// `.npy` or single-array `.npz` file, or raw bytes to compress.
        input: PathBuf,
        /// Archive to write. Removed if packing fails.
        output: PathBuf,
        /// Element type for raw input, as a NumPy descr such as `f4` or `<f8`.
        #[arg(long)]
        dtype: Option<String>,
        /// Shape for raw input, comma-separated, such as `1000,256`.
        #[arg(long, value_delimiter = ',')]
        shape: Option<Vec<u64>>,
        /// Chunk shape, comma-separated, one length per axis. Picks about 1 MiB per chunk if omitted.
        #[arg(long, value_delimiter = ',')]
        chunk: Option<Vec<u64>>,
        /// Filter for every chunk. `auto` tries each and keeps the smallest.
        #[arg(long, value_parser = ["auto", "none", "shuffle", "xor", "up", "delta", "delta-up", "lorenzo"], default_value = "auto")]
        filter: String,
        /// zstd compression level.
        #[arg(long, default_value_t = DEFAULT_LEVEL)]
        level: i32,
    },
    /// Write the whole array back out. A `.npy` archive gives the original file.
    UnpackArrays {
        /// Arrays archive or bundle to read.
        archive: PathBuf,
        /// Bundles only: the array to write, by name. Needed when there are several.
        #[arg(long)]
        member: Option<String>,
        /// File to write.
        output: PathBuf,
    },
    /// Write one rectangular slice of an arrays archive to stdout as raw bytes.
    ///
    /// Only the chunks the slice touches are decoded.
    SliceArrays {
        /// Arrays archive or bundle to read.
        archive: PathBuf,
        /// Bundles only: the array to read, by name. Needed when there are several.
        #[arg(long)]
        member: Option<String>,
        /// One range per axis, as `START:END` with END exclusive. Repeat the option for each axis.
        #[arg(long = "range", value_name = "START:END", required = true)]
        ranges: Vec<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Pack {
            input,
            output,
            chunk_size,
            content_defined,
            dictionary,
            level,
            encrypt,
            passphrase_file,
            recipient,
            recipient_file,
        } => {
            let encryption = encryption_choice(encrypt, passphrase_file, recipient, recipient_file);
            pack(
                &input,
                &output,
                chunk_size,
                if content_defined {
                    Chunking::ContentDefined
                } else {
                    Chunking::Fixed
                },
                dictionary,
                level,
                encryption,
            )
        }
        Command::Unpack {
            archive,
            output,
            passphrase_file,
            identity,
            max_output,
        } => unpack(
            &archive,
            &output,
            passphrase_file.as_deref(),
            identity.as_deref(),
            max_output,
        ),
        Command::Keygen { output } => keygen(&output),
        Command::Inspect {
            archive,
            chunks,
            tensors,
            passphrase_file,
            identity,
            verify,
        } => inspect(
            &archive,
            chunks,
            tensors,
            passphrase_file.as_deref(),
            identity.as_deref(),
            verify,
        ),
        Command::PackTensors {
            input,
            output,
            level,
            shard_size,
        } => match shard_size {
            Some(bytes) => pack_tensor_shards(&input, &output, bytes, level),
            None => pack_tensors(&input, &output, level),
        },
        Command::UnpackTensors { archive, output } => unpack_tensors(&archive, &output),
        Command::PackLogs {
            input,
            output,
            format,
            timestamp_field,
            block_size,
            level,
            no_dictionary,
            encrypt,
            passphrase_file,
            recipient,
            recipient_file,
        } => {
            let encryption = encryption_choice(encrypt, passphrase_file, recipient, recipient_file);
            pack_logs(
                &input,
                &output,
                LogsPackArgs {
                    format: format.as_deref(),
                    timestamp_field: &timestamp_field,
                    block_size,
                    level,
                    dictionary: !no_dictionary,
                },
                encryption,
            )
        }
        Command::PackGenomics {
            input,
            output,
            block_bases,
            level,
        } => pack_genomics(&input, &output, block_bases, level),
        Command::UnpackGenomics { archive, output } => unpack_genomics(&archive, &output),
        Command::PackArrays {
            input,
            output,
            dtype,
            shape,
            chunk,
            filter,
            level,
        } => pack_arrays(&input, &output, dtype, shape, chunk, &filter, level),
        Command::UnpackArrays {
            archive,
            member,
            output,
        } => unpack_arrays(&archive, member.as_deref(), &output),
        Command::SliceArrays {
            archive,
            member,
            ranges,
        } => slice_arrays(&archive, member.as_deref(), &ranges),
        Command::FetchGenomics {
            archive,
            id,
            start,
            end,
        } => fetch_genomics(&archive, &id, start.zip(end)),
        Command::Query {
            archive,
            name,
            from,
            to,
            columns,
            sql,
            output,
            passphrase_file,
            identity,
        } => query(
            &archive,
            name.as_deref(),
            from.as_deref(),
            to.as_deref(),
            columns,
            sql.as_deref(),
            output.as_deref(),
            Credentials {
                passphrase_file: passphrase_file.as_deref(),
                identity: identity.as_deref(),
            },
        ),
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
        Command::ImportDataset {
            input,
            output,
            level,
        } => import_dataset(&input, &output, level),
        Command::ListDataset { archive, verify } => list_dataset(&archive, verify),
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

/// How `pack` encrypts, if at all.
enum Encryption {
    /// No encryption.
    Off,
    /// A passphrase, from this file, or prompted for if `None`.
    Passphrase(Option<PathBuf>),
    /// Public keys given on the command line, and keys from this file.
    Recipients(Vec<String>, Option<PathBuf>),
}

/// The encryption asked for on the command line. Recipient keys take precedence
/// over a passphrase, and the two cannot be combined (clap refuses that).
fn encryption_choice(
    encrypt: bool,
    passphrase_file: Option<PathBuf>,
    recipient: Vec<String>,
    recipient_file: Option<PathBuf>,
) -> Encryption {
    if !recipient.is_empty() || recipient_file.is_some() {
        Encryption::Recipients(recipient, recipient_file)
    } else if encrypt || passphrase_file.is_some() {
        Encryption::Passphrase(passphrase_file)
    } else {
        Encryption::Off
    }
}

/// The passphrase, or the recipient keys, that an encryption choice resolves to.
type ResolvedKeys = (Option<String>, Option<Vec<x25519::Recipient>>);

/// Reads any passphrase file or recipient keys. Done before an output file is
/// created, so a bad key leaves nothing behind.
fn resolve_encryption(encryption: Encryption) -> Result<ResolvedKeys, Box<dyn Error>> {
    Ok(match encryption {
        Encryption::Off => (None, None),
        Encryption::Passphrase(file) => {
            let passphrase = match file {
                Some(file) => read_passphrase_file(&file)?,
                None => prompt_new_passphrase()?,
            };
            (Some(passphrase), None)
        }
        Encryption::Recipients(keys, file) => {
            (None, Some(read_recipients(&keys, file.as_deref())?))
        }
    })
}

fn pack(
    input: &Path,
    output: &Path,
    chunk_size: usize,
    chunking: Chunking,
    dictionary: Option<usize>,
    level: i32,
    encryption: Encryption,
) -> Result<String, Box<dyn Error>> {
    let opts = PackOptions {
        chunk_size,
        chunking,
        level,
        dictionary,
    };
    let reader = open_file(input, "packing")?;

    let (passphrase, recipients) = resolve_encryption(encryption)?;

    let writer = BufWriter::new(create(output)?);
    let stats = finish_or_remove(output, || match (&passphrase, &recipients) {
        (Some(p), _) => pack_encrypted(reader, writer, &opts, p),
        (None, Some(r)) => pack_for_recipients(reader, writer, &opts, r),
        (None, None) => archive::pack(reader, writer, &opts),
    })?;
    let encrypted = match (&passphrase, &recipients) {
        (Some(_), _) => ", encrypted",
        (None, Some(_)) => ", encrypted to public keys",
        (None, None) => "",
    };
    let ratio = stats.uncompressed_bytes as f64 / stats.archive_bytes.max(1) as f64;
    Ok(format!(
        "packed {} -> {} bytes ({ratio:.2}x, {} chunks{encrypted})",
        stats.uncompressed_bytes, stats.archive_bytes, stats.chunks
    ))
}

fn unpack(
    archive_path: &Path,
    output: &Path,
    passphrase_file: Option<&Path>,
    identity_file: Option<&Path>,
    max_output: Option<u64>,
) -> Result<String, Box<dyn Error>> {
    let kind = ArchiveKind::detect(archive_path)?;
    if kind != ArchiveKind::Generic {
        return Err(format!(
            "{} is a {} archive; use `{}`",
            archive_path.display(),
            kind.name(),
            kind.command()
        )
        .into());
    }
    let reader = open_archive_file(archive_path, passphrase_file, identity_file, "unpacking")?;
    let writer = BufWriter::new(create(output)?);

    let stats = finish_or_remove(output, || {
        archive::unpack(
            reader,
            writer,
            &UnpackOptions {
                max_total_size: max_output,
                ..UnpackOptions::default()
            },
        )
    })?;
    Ok(format!(
        "unpacked {} bytes from {} chunks",
        stats.uncompressed_bytes, stats.chunks
    ))
}

fn inspect(
    path: &Path,
    show_chunks: bool,
    list_tensors: bool,
    passphrase_file: Option<&Path>,
    identity_file: Option<&Path>,
    verify: bool,
) -> Result<String, Box<dyn Error>> {
    let kind = ArchiveKind::detect(path)?;
    let tensor_like = matches!(kind, ArchiveKind::Tensors | ArchiveKind::Sharded);
    if list_tensors && !tensor_like {
        return Err(format!(
            "--tensors applies to tensor archives, not {} archives",
            kind.name()
        )
        .into());
    }
    if show_chunks && kind != ArchiveKind::Generic {
        return Err(format!(
            "--chunks applies to generic archives, not {} archives",
            kind.name()
        )
        .into());
    }
    match kind {
        ArchiveKind::Logs => return inspect_logs(path),
        ArchiveKind::Tensors => return inspect_tensors(path, verify, list_tensors),
        ArchiveKind::Sharded => return inspect_sharded(path, verify, list_tensors),
        ArchiveKind::Assets => return inspect_assets(path, verify),
        ArchiveKind::Genomics => return inspect_genomics(path),
        ArchiveKind::Arrays | ArchiveKind::ArraysBundle => return inspect_arrays(path),
        ArchiveKind::Generic => {}
    }

    let reader = open_archive_file(path, passphrase_file, identity_file, "reading")?;
    // An encrypted logs archive looks generic until it is decrypted.
    let reader = match peek_kind(reader)? {
        Peeked::Logs(bytes) => {
            if show_chunks {
                return Err("--chunks applies to generic archives, not logs archives".into());
            }
            return inspect_logs_from(path, Cursor::new(bytes));
        }
        Peeked::Other(reader) => reader,
    };
    let inspection = archive::inspect(reader)?;
    let stats = &inspection.stats;

    let mut lines = vec![
        format!("archive:       {}", path.display()),
        format!("chunks:        {}", stats.chunks),
        format!("uncompressed:  {} bytes", stats.uncompressed_bytes),
        format!("archive size:  {} bytes", stats.archive_bytes),
        format!("ratio:         {:.2}x", inspection.ratio()),
    ];
    if inspection.dictionary_bytes > 0 {
        lines.push(format!(
            "dictionary:    {} bytes",
            inspection.dictionary_bytes
        ));
    }

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

/// The settings of `pack-logs` that are not the paths or the encryption.
struct LogsPackArgs<'a> {
    format: Option<&'a str>,
    timestamp_field: &'a str,
    block_size: usize,
    level: i32,
    dictionary: bool,
}

fn pack_logs(
    input: &Path,
    output: &Path,
    args: LogsPackArgs,
    encryption: Encryption,
) -> Result<String, Box<dyn Error>> {
    let format = match args.format {
        Some("csv") => Format::Csv,
        Some("prometheus") => Format::Prometheus,
        Some(_) => Format::Jsonl,
        None => match input.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("csv") => Format::Csv,
            Some(ext) if ext.eq_ignore_ascii_case("prom") => Format::Prometheus,
            _ => Format::Jsonl,
        },
    };
    let opts = logs::PackOptions {
        format,
        timestamp_field: args.timestamp_field.to_owned(),
        block_size: args.block_size,
        level: args.level,
        dictionary: args.dictionary,
    };

    let (passphrase, recipients) = resolve_encryption(encryption)?;
    let reader = open_file(input, "packing")?;
    let stats = write_logs(
        output,
        reader,
        &opts,
        passphrase.as_deref(),
        recipients.as_deref(),
    )?;

    let encrypted = match (&passphrase, &recipients) {
        (Some(_), _) => ", encrypted",
        (None, Some(_)) => ", encrypted to public keys",
        (None, None) => "",
    };
    let ratio = stats.input_bytes as f64 / stats.archive_bytes.max(1) as f64;
    Ok(format!(
        "packed {} records in {} blocks: {} -> {} bytes ({ratio:.2}x, {}-byte dictionary{encrypted})",
        stats.records, stats.blocks, stats.input_bytes, stats.archive_bytes, stats.dictionary_bytes
    ))
}

/// Writes a logs archive to `output`, encrypted if a passphrase or recipients are
/// given. The archive is encrypted as it is written, so no plaintext copy is
/// made. The output is removed if packing fails.
fn write_logs<R: BufRead + Seek>(
    output: &Path,
    reader: R,
    opts: &logs::PackOptions,
    passphrase: Option<&str>,
    recipients: Option<&[x25519::Recipient]>,
) -> Result<logs::Stats, Box<dyn Error>> {
    let run = || -> Result<logs::Stats, Box<dyn Error>> {
        let file = BufWriter::new(create(output)?);
        Ok(match (passphrase, recipients) {
            (Some(p), _) => {
                let mut encrypted = tpt_press_core::encrypt_with_passphrase(file, p)?;
                let stats = logs::pack(reader, &mut encrypted, opts)?;
                encrypted.finish()?.flush()?;
                stats
            }
            (None, Some(r)) => {
                let mut encrypted = tpt_press_core::encrypt_for_recipients(file, r)?;
                let stats = logs::pack(reader, &mut encrypted, opts)?;
                encrypted.finish()?.flush()?;
                stats
            }
            (None, None) => {
                let mut file = file;
                let stats = logs::pack(reader, &mut file, opts)?;
                file.flush()?;
                stats
            }
        })
    };
    run().inspect_err(|_| {
        let _ = fs::remove_file(output);
    })
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

/// Packs a model into shards in the directory `output`, with a manifest.
/// Removes the directory if packing fails, since this command created it.
fn pack_tensor_shards(
    input: &Path,
    output: &Path,
    shard_bytes: u64,
    level: i32,
) -> Result<String, Box<dyn Error>> {
    let reader = open_file(input, "packing")?;
    fs::create_dir(output).map_err(|e| format!("cannot create {}: {e}", output.display()))?;

    let opts = tensors::PackOptions { level };
    let result = (|| -> Result<(tensors::Manifest, usize), Box<dyn Error>> {
        let (manifest, stats) = tensors::pack_sharded(reader, shard_bytes, &opts, |name| {
            File::create(output.join(name)).map(BufWriter::new)
        })?;
        fs::write(output.join(tensors::MANIFEST_FILE), manifest.to_json())?;
        Ok((manifest, stats.len()))
    })();
    match result {
        Ok((manifest, shards)) => Ok(format!(
            "packed {} tensors into {shards} shards in {}",
            manifest.tensor_count(),
            output.display()
        )),
        Err(e) => {
            let _ = fs::remove_dir_all(output);
            Err(e)
        }
    }
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

fn inspect_tensors(path: &Path, verify: bool, list: bool) -> Result<String, Box<dyn Error>> {
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
    if list {
        lines.push(String::new());
        lines.push(format!(
            "{:<40} {:<8} {:<24} {:>14}",
            "tensor", "dtype", "shape", "bytes"
        ));
        for t in archive.tensors() {
            let shape = t
                .shape
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join("x");
            lines.push(format!(
                "{:<40} {:<8} {:<24} {:>14}",
                t.name, t.dtype, shape, t.byte_len
            ));
        }
    }
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

/// Converts a WebDataset tar into a dataset archive.
fn import_dataset(input: &Path, output: &Path, level: i32) -> Result<String, Box<dyn Error>> {
    let reader = BufReader::new(
        File::open(input).map_err(|e| format!("cannot read {}: {e}", input.display()))?,
    );
    let writer = BufWriter::new(create(output)?);
    let stats = finish_or_remove(output, || datasets::import_tar(reader, writer, level))?;
    Ok(format!(
        "imported {} samples ({} members), skipped {} non-member entries: {} -> {} bytes",
        stats.samples, stats.members, stats.skipped, stats.input_bytes, stats.archive_bytes
    ))
}

/// Lists the first samples of a dataset archive, and optionally checks them all.
fn list_dataset(archive: &Path, verify: bool) -> Result<String, Box<dyn Error>> {
    let reader = BufReader::new(
        File::open(archive).map_err(|e| format!("cannot read {}: {e}", archive.display()))?,
    );
    let mut dataset = datasets::Dataset::open(reader)?;
    let mut lines = vec![format!("{} samples", dataset.len())];
    for index in 0..dataset.len().min(LIST_LIMIT) {
        lines.push(format!(
            "{}  [{}]",
            dataset.key(index)?,
            dataset.member_names(index)?.join(", ")
        ));
    }
    if dataset.len() > LIST_LIMIT {
        lines.push(format!("... {} more", dataset.len() - LIST_LIMIT));
    }
    if verify {
        dataset.verify()?;
        lines.push("all checksums match".to_owned());
    }
    Ok(lines.join("\n"))
}

/// How many samples `list-dataset` prints.
const LIST_LIMIT: usize = 20;

/// `query` on a tensor archive outputs one tensor. On a logs archive it
/// outputs matching records, or the result of `--sql`.
// Eight arguments map one-to-one onto the subcommand's options.
#[allow(clippy::too_many_arguments)]
fn query(
    archive: &Path,
    name: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    columns: Option<Vec<String>>,
    sql: Option<&str>,
    output: Option<&Path>,
    creds: Credentials,
) -> Result<String, Box<dyn Error>> {
    if matches!(
        ArchiveKind::detect(archive)?,
        ArchiveKind::Tensors | ArchiveKind::Sharded
    ) {
        if from.is_some() || to.is_some() || columns.is_some() || sql.is_some() {
            return Err("--from, --to, --columns and --sql apply to logs archives".into());
        }
        let name = name.ok_or("a tensor archive needs a tensor name: `query <archive> <name>`")?;
        return query_tensor(archive, name, output);
    }
    if name.is_some() || output.is_some() {
        return Err("a tensor name and --output apply to tensor archives".into());
    }
    let query = logs_query(from, to, columns)?;
    match sql {
        Some(sql) => query_logs_sql(archive, &query, sql, creds),
        None => query_logs(archive, &query, creds),
    }
}

/// Writes one tensor's raw bytes to `output`, or to stdout. Its summary goes to stderr,
/// so stdout carries only the tensor.
fn query_tensor(
    archive_path: &Path,
    name: &str,
    output: Option<&Path>,
) -> Result<String, Box<dyn Error>> {
    // For a sharded archive, the tensor lives in one of its shard files.
    let archive_path = if archive_path.is_dir() {
        let manifest = read_manifest(archive_path)?;
        let shard = manifest
            .shards
            .iter()
            .find(|s| s.tensors.iter().any(|t| t == name))
            .ok_or_else(|| format!("no tensor named `{name}` in {}", archive_path.display()))?;
        archive_path.join(&shard.file)
    } else {
        archive_path.to_path_buf()
    };
    let archive_path = archive_path.as_path();
    let mut archive = tensors::Archive::open(open_file(archive_path, "reading")?)?;
    let bytes = archive
        .tensor(name)
        .ok_or_else(|| format!("no tensor named `{name}` in {}", archive_path.display()))?
        .byte_len;

    match output {
        Some(path) => {
            finish_or_remove(path, || -> tensors::Result<()> {
                let mut writer = BufWriter::new(File::create(path).map_err(tensors::Error::Io)?);
                archive.read_tensor_into(name, |piece| Ok(writer.write_all(piece)?))?;
                writer.flush()?;
                Ok(())
            })?;
        }
        None => {
            let stdout = io::stdout();
            let mut writer = BufWriter::new(stdout.lock());
            archive.read_tensor_into(name, |piece| Ok(writer.write_all(piece)?))?;
            writer.flush()?;
        }
    }
    eprintln!("tensor {name}: {bytes} bytes");
    Ok(String::new())
}

fn logs_query(
    from: Option<&str>,
    to: Option<&str>,
    columns: Option<Vec<String>>,
) -> Result<Query, Box<dyn Error>> {
    Ok(Query {
        from: from.map(logs::parse_timestamp).transpose()?,
        to: to.map(logs::parse_timestamp).transpose()?,
        columns,
    })
}

/// Runs `sql` over the matching records of a logs archive, and prints the result table.
#[cfg(feature = "strata")]
fn query_logs_sql(
    archive_path: &Path,
    query: &Query,
    sql: &str,
    creds: Credentials,
) -> Result<String, Box<dyn Error>> {
    // As with `query`, an encrypted archive is decrypted into memory first.
    let table = if file_is_encrypted(archive_path)? {
        let bytes = decrypt_logs(archive_path, creds)?;
        logs::query_sql(Cursor::new(bytes), query, sql)?
    } else {
        logs::query_sql(open_file(archive_path, "reading")?, query, sql)?
    };
    tpt_strata::print_table(&table);
    Ok(String::new())
}

#[cfg(not(feature = "strata"))]
fn query_logs_sql(
    _archive_path: &Path,
    _query: &Query,
    _sql: &str,
    _creds: Credentials,
) -> Result<String, Box<dyn Error>> {
    Err("--sql needs a build with the `strata` feature: cargo build --features strata".into())
}

fn query_logs(
    archive_path: &Path,
    query: &Query,
    creds: Credentials,
) -> Result<String, Box<dyn Error>> {
    let stdout = io::stdout();
    // A large buffer means fewer writes to the pipe, which is much faster when
    // the output is piped into another program.
    let mut writer = BufWriter::with_capacity(1024 * 1024, stdout.lock());
    // An encrypted archive is decrypted into memory first, since a query needs
    // to seek. Nothing is written to disk.
    let result = if file_is_encrypted(archive_path)? {
        let bytes = decrypt_logs(archive_path, creds)?;
        logs::query(Cursor::new(bytes), &mut writer, query)
    } else {
        logs::query(open_file(archive_path, "reading")?, &mut writer, query)
    };
    let stats = match result {
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

/// How to open an encrypted archive: a passphrase file, or an identity file.
/// With neither, a passphrase is prompted for.
#[derive(Clone, Copy)]
struct Credentials<'a> {
    passphrase_file: Option<&'a Path>,
    identity: Option<&'a Path>,
}

/// A decrypted stream, peeked at to find out what it holds.
enum Peeked {
    /// A logs archive, read in full.
    Logs(Vec<u8>),
    /// Anything else. The reader still yields the whole stream.
    Other(Box<dyn Read>),
}

/// Reads the first four bytes of `reader` to find out whether it is a logs
/// archive. A logs archive is read in full, since it must be seekable.
fn peek_kind(mut reader: Box<dyn Read>) -> io::Result<Peeked> {
    let mut head = Vec::with_capacity(4);
    (&mut reader).take(4).read_to_end(&mut head)?;
    if head == b"TPLG" {
        reader.read_to_end(&mut head)?;
        return Ok(Peeked::Logs(head));
    }
    Ok(Peeked::Other(Box::new(Cursor::new(head).chain(reader))))
}

/// Whether the file at `path` is an age stream, which hides its archive type.
fn file_is_encrypted(path: &Path) -> Result<bool, Box<dyn Error>> {
    let mut head = Vec::new();
    open_file(path, "reading")?
        .take(22)
        .read_to_end(&mut head)?;
    Ok(tpt_press_core::looks_encrypted(&head))
}

/// Decrypts a logs archive into memory. Fails if the encrypted archive holds
/// something else.
fn decrypt_logs(path: &Path, creds: Credentials) -> Result<Vec<u8>, Box<dyn Error>> {
    let reader = open_archive_file(path, creds.passphrase_file, creds.identity, "reading")?;
    match peek_kind(reader)? {
        Peeked::Logs(bytes) => Ok(bytes),
        Peeked::Other(_) => Err(format!(
            "{} is an encrypted archive, but not a logs archive",
            path.display()
        )
        .into()),
    }
}

/// The archive types tpt-press writes. Each has its own magic bytes at the start of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveKind {
    /// Core archive from `pack`. Also the fallback for anything unrecognised,
    /// which then fails with a parse error. Encrypted archives land here too,
    /// since they start with age's magic.
    Generic,
    /// Logs archive from `pack-logs`. Never encrypted.
    Logs,
    /// Tensor archive from `pack-tensors`.
    Tensors,
    /// Assets archive from `pack-assets`.
    Assets,
    /// A directory of tensor shards and a manifest, from `pack-tensors --shard-size`.
    Sharded,
    /// Genomics archive from `pack-genomics`.
    Genomics,
    /// Arrays archive from `pack-arrays`.
    Arrays,
    /// Bundle of named arrays, from `pack-arrays` on a `.npz` with several arrays.
    ArraysBundle,
}

impl ArchiveKind {
    /// Identifies an archive from the first four bytes of its file. A directory
    /// is a sharded archive.
    fn detect(path: &Path) -> Result<Self, Box<dyn Error>> {
        if path.is_dir() {
            return Ok(Self::Sharded);
        }
        let file = open_file(path, "reading")?;
        let mut magic = Vec::new();
        file.take(4).read_to_end(&mut magic)?;
        Ok(if LogsArchive::recognises(&magic) {
            Self::Logs
        } else if TensorsArchive::recognises(&magic) {
            Self::Tensors
        } else if AssetsArchive::recognises(&magic) {
            Self::Assets
        } else if GenomicsArchive::recognises(&magic) {
            Self::Genomics
        } else if ArraysArchive::recognises(&magic) {
            Self::Arrays
        } else if arrays::is_bundle(&magic) {
            Self::ArraysBundle
        } else {
            Self::Generic
        })
    }

    /// A short name for error messages.
    fn name(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Logs => "logs",
            Self::Tensors => "tensors",
            Self::Assets => "assets",
            Self::Sharded => "sharded tensors",
            Self::Genomics => "genomics",
            Self::Arrays => "arrays",
            Self::ArraysBundle => "arrays bundle",
        }
    }

    /// The subcommand that works on this archive type.
    fn command(self) -> &'static str {
        match self {
            Self::Generic => "unpack",
            Self::Logs => "query",
            Self::Tensors => "unpack-tensors",
            Self::Assets => "unpack-assets",
            Self::Sharded => "query <directory> <name>",
            Self::Genomics => "unpack-genomics",
            Self::Arrays => "unpack-arrays",
            Self::ArraysBundle => "unpack-arrays --member NAME",
        }
    }
}

/// Reads and checks the manifest of a sharded archive directory.
fn read_manifest(dir: &Path) -> Result<tensors::Manifest, Box<dyn Error>> {
    let path = dir.join(tensors::MANIFEST_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|e| format!("cannot read manifest {}: {e}", path.display()))?;
    Ok(tensors::Manifest::parse(&text)?)
}

fn inspect_sharded(dir: &Path, verify: bool, list: bool) -> Result<String, Box<dyn Error>> {
    let manifest = read_manifest(dir)?;
    let mut lines = vec![
        format!("archive:        {}", dir.display()),
        format!("shards:         {}", manifest.shards.len()),
        format!("tensors:        {}", manifest.tensor_count()),
    ];
    let mut total = 0u64;
    for shard in &manifest.shards {
        let bytes = file_len(&dir.join(&shard.file))?;
        total += bytes;
        lines.push(format!(
            "  {:<20} {:>6} tensors, {bytes} bytes",
            shard.file,
            shard.tensors.len()
        ));
    }
    lines.push(format!("archive size:   {total} bytes"));

    if list {
        lines.push(String::new());
        lines.push(format!(
            "{:<40} {:<8} {:<24} {:>14}",
            "tensor", "dtype", "shape", "bytes"
        ));
        for shard in &manifest.shards {
            let archive = tensors::Archive::open(open_file(&dir.join(&shard.file), "reading")?)?;
            for t in archive.tensors() {
                let shape = t
                    .shape
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join("x");
                lines.push(format!(
                    "{:<40} {:<8} {:<24} {:>14}",
                    t.name, t.dtype, shape, t.byte_len
                ));
            }
        }
    }
    if verify {
        for shard in &manifest.shards {
            let mut archive =
                tensors::Archive::open(open_file(&dir.join(&shard.file), "reading")?)?;
            archive.verify()?;
        }
        lines.push("all checksums match".to_owned());
    }
    Ok(lines.join("\n"))
}

fn inspect_assets(path: &Path, verify: bool) -> Result<String, Box<dyn Error>> {
    let archive = assets::Archive::open(path)?;
    let entries = archive.entries();
    let raw: u64 = entries.iter().map(|e| e.raw_len).sum();
    let stored: u64 = entries.iter().map(|e| e.stored_len).sum();
    let raw_stored = entries
        .iter()
        .filter(|e| e.compression == assets::Compression::Stored)
        .count();

    let mut lines = vec![
        format!("archive:        {}", path.display()),
        format!("assets:         {}", entries.len()),
        format!("  raw         {raw_stored} assets, used in place"),
        format!("  zstd        {} assets", entries.len() - raw_stored),
        format!("uncompressed:   {raw} bytes"),
        format!("stored:         {stored} bytes"),
        format!("archive size:   {} bytes", fs::metadata(path)?.len()),
    ];
    if verify {
        archive.verify()?;
        lines.push(format!("all {} checksums match", entries.len()));
    }
    Ok(lines.join("\n"))
}

/// Packs FASTA or FASTQ into a genomics archive, and reports the ratio.
fn pack_genomics(
    input: &Path,
    output: &Path,
    block_bases: usize,
    level: i32,
) -> Result<String, Box<dyn Error>> {
    let options = genomics::PackOptions { block_bases, level };
    let reader = open_file(input, "packing")?;
    let writer = BufWriter::new(create(output)?);
    let stats = finish_or_remove(output, || genomics::pack(reader, writer, &options))?;
    Ok(format!(
        "packed {} {} records, {} bases in {} blocks: {} -> {} bytes ({:.2}x)",
        stats.records,
        stats.kind.name(),
        stats.bases,
        stats.blocks,
        stats.input_bytes,
        stats.output_bytes,
        stats.input_bytes as f64 / stats.output_bytes.max(1) as f64
    ))
}

fn unpack_genomics(archive: &Path, output: &Path) -> Result<String, Box<dyn Error>> {
    let reader = open_file(archive, "unpacking")?;
    let writer = BufWriter::new(create(output)?);
    let summary = finish_or_remove(output, || genomics::unpack(reader, writer))?;
    Ok(format!(
        "unpacked {} {} records, {} bases",
        summary.records,
        summary.kind.name(),
        summary.bases
    ))
}

/// Writes one record, or a range of it, to stdout. The summary goes to stderr.
fn fetch_genomics(
    archive: &Path,
    id: &str,
    range: Option<(u64, u64)>,
) -> Result<String, Box<dyn Error>> {
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let stats = genomics::fetch(open_file(archive, "reading")?, id, range, &mut writer)?;
    eprintln!(
        "record {id}: {} bases; read {} blocks",
        stats.record_length, stats.blocks_read
    );
    Ok(String::new())
}

/// Packs a `.npy` file, or raw bytes given with `--dtype` and `--shape`.
fn pack_arrays(
    input: &Path,
    output: &Path,
    dtype: Option<String>,
    shape: Option<Vec<u64>>,
    chunk: Option<Vec<u64>>,
    filter: &str,
    level: i32,
) -> Result<String, Box<dyn Error>> {
    if input.is_dir() {
        // A Zarr v2 array is a directory with a .zarray file.
        let options = arrays::PackOptions {
            chunk_shape: chunk,
            level,
            filter: filter_choice(filter),
        };
        let writer = BufWriter::new(create(output)?);
        let stats = finish_or_remove(output, || arrays::pack_zarr(input, writer, &options))?;
        return Ok(describe_packed(Some("zarr"), &stats));
    }
    let mut head = [0u8; 6];
    let read = File::open(input)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| format!("cannot read {}: {e}", input.display()))?;
    let npz = arrays::is_npz(&head[..read]);
    let npy = npz || arrays::is_npy(&head[..read]);
    let raw = if npy {
        None
    } else {
        let dtype = dtype.ok_or("raw input needs --dtype, such as f4 or <f8")?;
        let shape = shape.ok_or("raw input needs --shape, such as 1000,256")?;
        Some((dtype, shape))
    };

    let options = arrays::PackOptions {
        chunk_shape: chunk,
        level,
        filter: filter_choice(filter),
    };
    let reader = open_file(input, "packing")?;
    let writer = BufWriter::new(create(output)?);
    let packed: Vec<(Option<String>, arrays::Stats)> = match raw {
        None if npz => finish_or_remove(output, || arrays::pack_npz(reader, writer, &options))?
            .into_iter()
            .map(|(name, stats)| (Some(name), stats))
            .collect(),
        None => vec![(
            None,
            finish_or_remove(output, || arrays::pack_npy(reader, writer, &options))?,
        )],
        Some((dtype, shape)) => vec![(
            None,
            finish_or_remove(output, || {
                arrays::pack_raw(reader, writer, &dtype, &shape, &options)
            })?,
        )],
    };
    Ok(packed
        .iter()
        .map(|(name, stats)| describe_packed(name.as_deref(), stats))
        .collect::<Vec<_>>()
        .join("\n"))
}
/// The filter option, as a choice for the writer. Unknown names mean automatic.
fn filter_choice(name: &str) -> arrays::FilterChoice {
    match name {
        "none" => arrays::FilterChoice::Fixed(arrays::Filter::None),
        "shuffle" => arrays::FilterChoice::Fixed(arrays::Filter::Shuffle),
        "xor" => arrays::FilterChoice::Fixed(arrays::Filter::XorShuffle),
        "up" => arrays::FilterChoice::Fixed(arrays::Filter::UpShuffle),
        "delta" => arrays::FilterChoice::Fixed(arrays::Filter::Delta),
        "delta-up" => arrays::FilterChoice::Fixed(arrays::Filter::DeltaUp),
        "lorenzo" => arrays::FilterChoice::Fixed(arrays::Filter::Lorenzo),
        _ => arrays::FilterChoice::Auto,
    }
}

/// One line describing a packed array.
fn describe_packed(name: Option<&str>, stats: &arrays::Stats) -> String {
    let [none, shuffle, xor, up, delta, delta_up, lorenzo] = stats.filter_counts;
    let label = name.map(|n| format!("{n}: ")).unwrap_or_default();
    format!(
        "{label}packed {} {:?} in {} chunks of {:?}: {} -> {} bytes ({:.2}x); filters: none {none}, shuffle {shuffle}, xor+shuffle {xor}, xor-up+shuffle {up}, delta+shuffle {delta}, delta-up+shuffle {delta_up}, lorenzo+shuffle {lorenzo}",
        stats.dtype,
        stats.shape,
        stats.chunks,
        stats.chunk_shape,
        stats.input_bytes,
        stats.output_bytes,
        stats.input_bytes as f64 / stats.output_bytes.max(1) as f64
    )
}

/// Opens an arrays archive, or one member of a bundle, as a seekable reader.
/// A bundle with several arrays needs `member`.
fn arrays_input(
    path: &Path,
    member: Option<&str>,
    label: &'static str,
) -> Result<Box<dyn ReadSeek>, Box<dyn Error>> {
    let mut magic = [0u8; 4];
    let read = open_file(path, label)?.read(&mut magic)?;
    if !arrays::is_bundle(&magic[..read]) {
        if member.is_some() {
            return Err("--member applies to bundles of several arrays".into());
        }
        return Ok(Box::new(open_file(path, label)?));
    }
    let names: Vec<String> = arrays::bundle_members(open_file(path, label)?)?
        .into_iter()
        .map(|e| e.name)
        .collect();
    let name = match (member, names.as_slice()) {
        (Some(name), _) => name.to_owned(),
        (None, [only]) => only.clone(),
        (None, _) => {
            return Err(format!(
                "this bundle holds {} arrays; choose one with --member: {}",
                names.len(),
                names.join(", ")
            )
            .into())
        }
    };
    let bytes = arrays::read_member(open_file(path, label)?, &name)?;
    Ok(Box::new(Cursor::new(bytes)))
}

/// A seekable reader, as used for arrays archives.
trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

fn unpack_arrays(
    archive: &Path,
    member: Option<&str>,
    output: &Path,
) -> Result<String, Box<dyn Error>> {
    let reader = arrays_input(archive, member, "unpacking")?;
    let writer = BufWriter::new(create(output)?);
    let summary = finish_or_remove(output, || arrays::unpack(reader, writer))?;
    Ok(format!(
        "unpacked {} {:?}{}",
        summary.dtype,
        summary.shape,
        if summary.npy { " as a .npy file" } else { "" }
    ))
}

/// Parses `START:END` ranges, one per axis.
fn parse_ranges(specs: &[String]) -> Result<Vec<std::ops::Range<u64>>, Box<dyn Error>> {
    specs
        .iter()
        .map(|spec| {
            let (start, end) = spec
                .split_once(':')
                .ok_or_else(|| format!("bad range `{spec}`; use START:END"))?;
            let start: u64 = start
                .parse()
                .map_err(|_| format!("bad start in `{spec}`"))?;
            let end: u64 = end.parse().map_err(|_| format!("bad end in `{spec}`"))?;
            Ok(start..end)
        })
        .collect()
}

/// Writes one slice to stdout as raw bytes. The summary goes to stderr.
fn slice_arrays(
    archive: &Path,
    member: Option<&str>,
    specs: &[String],
) -> Result<String, Box<dyn Error>> {
    let ranges = parse_ranges(specs)?;
    let slice = arrays::read_slice(arrays_input(archive, member, "reading")?, &ranges)?;
    io::stdout().lock().write_all(&slice.bytes)?;
    eprintln!(
        "slice {:?}: {} bytes; decoded {} chunks",
        slice.shape,
        slice.bytes.len(),
        slice.chunks_read
    );
    Ok(String::new())
}

fn inspect_arrays(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut magic = [0u8; 4];
    let read = open_file(path, "reading")?.read(&mut magic)?;
    if arrays::is_bundle(&magic[..read]) {
        let members = arrays::bundle_members(open_file(path, "reading")?)?;
        let mut lines = vec![
            format!("archive:       {}", path.display()),
            format!("bundle of {} arrays:", members.len()),
        ];
        for member in members {
            lines.push(format!("  {:<16} {} bytes", member.name, member.length));
        }
        return Ok(lines.join("\n"));
    }
    let summary = arrays::inspect(open_file(path, "reading")?)?;
    Ok([
        format!("archive:       {}", path.display()),
        format!("dtype:         {}", summary.dtype),
        format!("shape:         {:?}", summary.shape),
        format!("chunk shape:   {:?}", summary.chunk_shape),
        format!("chunks:        {}", summary.chunks),
        format!("raw size:      {} bytes", summary.raw_bytes),
        format!("archive size:  {} bytes", summary.archive_bytes),
        format!(
            "npy header:    {}",
            if summary.npy { "kept" } else { "none" }
        ),
    ]
    .join("\n"))
}

fn inspect_genomics(path: &Path) -> Result<String, Box<dyn Error>> {
    let summary = genomics::inspect(open_file(path, "reading")?)?;
    Ok([
        format!("archive:       {}", path.display()),
        format!("format:        {}", summary.kind.name()),
        format!("records:       {}", summary.records),
        format!("bases:         {}", summary.bases),
        format!("blocks:        {}", summary.blocks),
        format!("archive size:  {} bytes", summary.archive_bytes),
    ]
    .join("\n"))
}

fn inspect_logs(path: &Path) -> Result<String, Box<dyn Error>> {
    inspect_logs_from(path, open_file(path, "reading")?)
}

/// Summarises a logs archive read from `reader`. `path` is only shown in the output.
fn inspect_logs_from<R: Read + Seek>(path: &Path, reader: R) -> Result<String, Box<dyn Error>> {
    let summary = logs::inspect(reader)?;
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
        Format::Prometheus => "prometheus",
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
/// `identity_file` for public-key encryption, or `passphrase_file` for a
/// passphrase. With neither, the user is prompted for a passphrase.
fn open_archive_file(
    path: &Path,
    passphrase_file: Option<&Path>,
    identity_file: Option<&Path>,
    label: &'static str,
) -> Result<Box<dyn Read>, Box<dyn Error>> {
    if let Some(file) = identity_file {
        let identities = read_identities(file)?;
        return Ok(open_with_identities(open_file(path, label)?, &identities)?);
    }
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

/// Writes a new identity file and returns its public key. Refuses to overwrite
/// an existing file, since that would destroy a key.
fn keygen(output: &Path) -> Result<String, Box<dyn Error>> {
    let identity = x25519::Identity::generate();
    let public = identity.to_public();

    let mut options = File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(output)
        .map_err(|e| format!("cannot create {}: {e}", output.display()))?;
    writeln!(file, "# created by tpt-press")?;
    writeln!(file, "# public key: {public}")?;
    writeln!(file, "{}", identity.to_string().expose_secret())?;
    Ok(format!(
        "wrote identity to {}\npublic key: {public}",
        output.display()
    ))
}

/// Reads the identities in an identity file, one per line. Blank lines and
/// lines starting with `#` are skipped.
fn read_identities(path: &Path) -> Result<Vec<x25519::Identity>, Box<dyn Error>> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("cannot read identity file {}: {e}", path.display()))?;
    let identities = key_lines(&text)
        .map(|line| {
            x25519::Identity::from_str(line)
                .map_err(|e| format!("bad identity in {}: {e}", path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if identities.is_empty() {
        return Err(format!("identity file {} has no keys", path.display()).into());
    }
    Ok(identities)
}

/// The public keys given on the command line, plus those in `file`.
fn read_recipients(
    keys: &[String],
    file: Option<&Path>,
) -> Result<Vec<x25519::Recipient>, Box<dyn Error>> {
    let mut recipients = Vec::new();
    let from_file = match file {
        Some(file) => fs::read_to_string(file)
            .map_err(|e| format!("cannot read recipient file {}: {e}", file.display()))?,
        None => String::new(),
    };
    for key in keys.iter().map(String::as_str).chain(key_lines(&from_file)) {
        let recipient = x25519::Recipient::from_str(key)
            .map_err(|e| format!("bad recipient key `{key}`: {e}"))?;
        recipients.push(recipient);
    }
    if recipients.is_empty() {
        return Err("no recipient keys given".into());
    }
    Ok(recipients)
}

/// The non-blank lines of `text` that are not comments, trimmed.
fn key_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
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
