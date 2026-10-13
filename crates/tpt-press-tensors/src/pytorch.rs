//! Reads PyTorch checkpoints saved by `torch.save` in the zip format.
//!
//! A checkpoint is a zip file. Its `data.pkl` entry holds the object graph, and
//! each storage's bytes are a separate entry, `<prefix>data/<key>`. Storage
//! entries are written uncompressed, so each tensor is a single run of bytes in
//! the file, and the tensor model can point at it directly.
//!
//! Only tensors are accepted. A checkpoint with other values in it, such as an
//! epoch number, is refused, since archiving it would silently drop them.
//! Tensors that are not contiguous in their storage are refused too.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::rc::Rc;

use zip::{CompressionMethod, ZipArchive};

use crate::dtype;
use crate::error::{map_io, Error, Result};
use crate::model::{Model, TensorInfo};
use crate::pickle::{self, Storage, TensorRef, Value, MAX_DEPTH};

/// Parses a `torch.save` zip checkpoint into a model whose tensors point at
/// absolute byte ranges of the file.
pub fn parse<R: Read + Seek>(input: &mut R) -> Result<Model> {
    input.seek(SeekFrom::Start(0)).map_err(map_io)?;
    let file_len = input.seek(SeekFrom::End(0)).map_err(map_io)?;
    input.seek(SeekFrom::Start(0)).map_err(map_io)?;

    let mut zip = ZipArchive::new(&mut *input).map_err(zip_error)?;
    let pickle_name = find_pickle(&zip)?;
    let prefix = pickle_name
        .strip_suffix("data.pkl")
        .unwrap_or_default()
        .to_owned();

    let pickle_bytes = read_entry(&mut zip, &pickle_name)?;
    let root = pickle::load(&pickle_bytes)?;

    let mut found = Vec::new();
    match root {
        Value::Dict(seq) => collect(&seq.items(), "", 1, &mut found)?,
        _ => {
            return Err(invalid(
                "the checkpoint is not a dict of tensors (a whole-model save is not supported)",
            ))
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    if found.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err(invalid("two tensors have the same name"));
    }

    // Storage entries are located once each, however many tensors share them.
    let mut storages: HashMap<String, (u64, u64)> = HashMap::new();
    let mut tensors = Vec::with_capacity(found.len());
    for (name, tensor) in &found {
        let storage = &tensor.storage;
        let (start, size) = match storages.get(&storage.key) {
            Some(&location) => location,
            None => {
                let location = storage_location(&mut zip, &prefix, &storage.key)?;
                storages.insert(storage.key.clone(), location);
                location
            }
        };
        tensors.push(tensor_info(name, tensor, start, size)?);
    }
    drop(zip);

    let mut metadata = std::collections::BTreeMap::new();
    metadata.insert("format".to_owned(), "pytorch".to_owned());
    Ok(Model {
        metadata,
        tensors,
        data_start: 0,
        data_len: file_len,
    })
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidModel(format!("PyTorch checkpoint: {}", message.into()))
}

fn zip_error(e: zip::result::ZipError) -> Error {
    invalid(format!("not a zip checkpoint ({e})"))
}

/// The `data.pkl` entry. Its directory prefix is the checkpoint's archive name,
/// so it can be anything, and the entry must be unique.
fn find_pickle<R: Read + Seek>(zip: &ZipArchive<R>) -> Result<String> {
    let mut found: Vec<String> = zip
        .file_names()
        .filter(|name| *name == "data.pkl" || name.ends_with("/data.pkl"))
        .map(str::to_owned)
        .collect();
    match found.len() {
        0 => Err(invalid("no data.pkl entry")),
        1 => Ok(found.remove(0)),
        _ => Err(invalid("more than one data.pkl entry")),
    }
}

/// Reads a whole stored entry. A deflated entry is refused, since it cannot be
/// read in place.
fn read_entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<Vec<u8>> {
    let mut entry = zip.by_name(name).map_err(zip_error)?;
    if entry.compression() != CompressionMethod::Stored {
        return Err(invalid(format!("entry `{name}` is compressed")));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes).map_err(map_io)?;
    Ok(bytes)
}

/// Where a storage's bytes lie in the file: its start and its length.
fn storage_location<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    prefix: &str,
    key: &str,
) -> Result<(u64, u64)> {
    let name = format!("{prefix}data/{key}");
    let entry = zip.by_name(&name).map_err(zip_error)?;
    if entry.compression() != CompressionMethod::Stored {
        return Err(invalid(format!(
            "storage `{key}` is compressed, and only uncompressed storages can be packed in place"
        )));
    }
    Ok((entry.data_start(), entry.size()))
}

/// Flattens the state dict into named tensors. Nested dicts get dotted names.
fn collect(
    items: &[Value],
    prefix: &str,
    depth: usize,
    out: &mut Vec<(String, Rc<TensorRef>)>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(invalid("the state dict is nested too deeply"));
    }
    for pair in items.chunks(2) {
        let [key, value] = pair else {
            return Err(invalid("dict has an unpaired key"));
        };
        let key = match key {
            Value::Str(s) => s.to_string(),
            _ => return Err(invalid("dict keys must be strings")),
        };
        let name = if prefix.is_empty() {
            key
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Tensor(t) => out.push((name, t.clone())),
            Value::Dict(seq) => collect(&seq.items(), &name, depth + 1, out)?,
            other => {
                return Err(invalid(format!(
                    "`{name}` is {}, not a tensor; only tensors can be archived",
                    other.describe()
                )))
            }
        }
    }
    Ok(())
}

/// Describes one tensor as an absolute byte range of the file.
fn tensor_info(name: &str, tensor: &TensorRef, start: u64, size: u64) -> Result<TensorInfo> {
    let dtype = tensor.storage.dtype;
    let elem = dtype::layout(dtype)
        .map(|l| l.block_bytes)
        .ok_or_else(|| Error::UnsupportedDtype(dtype.to_owned()))?;

    if size != tensor.storage.numel.saturating_mul(elem) {
        return Err(invalid(format!(
            "storage for `{name}` does not match its element count"
        )));
    }

    // A tensor is read as one run of bytes, so it must be row-major and contiguous.
    let mut expected = 1u64;
    for (dim, stride) in tensor.shape.iter().zip(&tensor.stride).rev() {
        if *dim != 1 && *stride != expected {
            return Err(invalid(format!(
                "`{name}` is not contiguous in its storage, and cannot be packed in place"
            )));
        }
        expected = expected
            .checked_mul(*dim)
            .ok_or_else(|| invalid("tensor is too large"))?;
    }
    let count = expected;

    // The view must lie inside its storage.
    let end_element = if count == 0 {
        tensor.offset
    } else {
        tensor
            .offset
            .checked_add(count)
            .ok_or_else(|| invalid("tensor is too large"))?
    };
    if end_element > tensor.storage.numel {
        return Err(invalid(format!(
            "`{name}` reads past the end of its storage"
        )));
    }

    let begin = start + tensor.offset * elem;
    Ok(TensorInfo {
        name: name.to_owned(),
        dtype: dtype.to_owned(),
        shape: tensor.shape.clone(),
        begin,
        end: begin + count * elem,
    })
}

/// Parses a legacy PyTorch checkpoint, the format `torch.save` wrote before the
/// zip format (PyTorch 1.5 and earlier). The file is a run of pickles, then the
/// raw data of each storage, each storage preceded by its element count. Only
/// the pickles are read into memory. The storage data is located by seeking,
/// so a large checkpoint never has to fit in memory.
pub fn parse_legacy<R: Read + Seek>(input: &mut R) -> Result<Model> {
    let file_len = input.seek(SeekFrom::End(0)).map_err(map_io)?;
    let header = read_legacy_header(input, file_len)?;

    // Each storage's data follows the header, in the order of the key list.
    let mut storages: HashMap<String, Rc<Storage>> = header
        .main
        .storages
        .iter()
        .map(|s| (s.key.clone(), s.clone()))
        .collect();
    let mut locations: HashMap<String, (u64, u64)> = HashMap::new();
    let mut pos = header.data_pos;
    for key in &header.keys {
        let storage = storages
            .remove(key)
            .ok_or_else(|| invalid(format!("storage `{key}` is listed but never referenced")))?;
        input.seek(SeekFrom::Start(pos)).map_err(map_io)?;
        let mut count = [0u8; 8];
        input.read_exact(&mut count).map_err(map_io)?;
        if u64::from_le_bytes(count) != storage.numel {
            return Err(invalid(format!(
                "storage `{key}` has the wrong element count"
            )));
        }
        let elem = dtype::layout(storage.dtype)
            .map(|l| l.block_bytes)
            .ok_or_else(|| Error::UnsupportedDtype(storage.dtype.to_owned()))?;
        let start = pos + 8;
        let size = storage
            .numel
            .checked_mul(elem)
            .ok_or_else(|| invalid("storage is too large"))?;
        pos = start
            .checked_add(size)
            .filter(|&end| end <= file_len)
            .ok_or_else(|| invalid(format!("storage `{key}` runs past the end of the file")))?;
        locations.insert(key.clone(), (start, size));
    }

    let mut found = Vec::new();
    match &header.main.value {
        Value::Dict(seq) => collect(&seq.items(), "", 1, &mut found)?,
        _ => {
            return Err(invalid(
                "the checkpoint is not a dict of tensors (a whole-model save is not supported)",
            ))
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    if found.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err(invalid("two tensors have the same name"));
    }

    let mut tensors = Vec::with_capacity(found.len());
    for (name, tensor) in &found {
        let (start, size) = *locations.get(&tensor.storage.key).ok_or_else(|| {
            invalid(format!(
                "`{name}` refers to a storage that is not in the file"
            ))
        })?;
        tensors.push(tensor_info(name, tensor, start, size)?);
    }

    let mut metadata = std::collections::BTreeMap::new();
    metadata.insert("format".to_owned(), "pytorch-legacy".to_owned());
    Ok(Model {
        metadata,
        tensors,
        data_start: 0,
        data_len: file_len,
    })
}

/// The pickles at the start of a legacy checkpoint.
struct LegacyHeader {
    /// The object graph, with the storages it refers to.
    main: pickle::Prefix,
    /// Storage keys, in the order their data appears in the file.
    keys: Vec<String>,
    /// Where the first storage's data count starts.
    data_pos: u64,
}

/// Reads the pickles from the start of the file. It reads only as much as the
/// pickles need, doubling the buffer until they parse.
fn read_legacy_header<R: Read + Seek>(input: &mut R, file_len: u64) -> Result<LegacyHeader> {
    let mut cap: u64 = 1 << 20;
    loop {
        let want = cap.min(file_len);
        input.seek(SeekFrom::Start(0)).map_err(map_io)?;
        let mut buf = vec![0u8; want as usize];
        input.read_exact(&mut buf).map_err(map_io)?;
        match parse_legacy_pickles(&buf) {
            Ok(header) => return Ok(header),
            Err(_) if want < file_len => cap = cap.saturating_mul(2),
            Err(e) => return Err(e),
        }
    }
}

fn parse_legacy_pickles(buf: &[u8]) -> Result<LegacyHeader> {
    let mut pos = 0usize;
    let mut next = || -> Result<pickle::Prefix> {
        let prefix = pickle::load_prefix(&buf[pos..])?;
        pos += prefix.consumed;
        Ok(prefix)
    };

    // The magic number, then the protocol version, then system information.
    next()?;
    match next()?.value {
        Value::Int(1001) => {}
        _ => return Err(invalid("unsupported legacy protocol version")),
    }
    next()?;

    let main = next()?;
    let keys = match next()?.value {
        Value::List(seq) => seq
            .items()
            .iter()
            .map(|key| match key {
                Value::Str(s) => Ok(s.to_string()),
                other => Err(invalid(format!("storage key is {}", other.describe()))),
            })
            .collect::<Result<Vec<_>>>()?,
        _ => return Err(invalid("the storage key list is missing")),
    };
    Ok(LegacyHeader {
        main,
        keys,
        data_pos: pos as u64,
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    use super::*;

    /// The pickle for `torch._utils._rebuild_tensor_v2` over storage `key`.
    fn tensor_pickle(key: &str, offset: u8, shape: &[u8], stride: &[u8]) -> Vec<u8> {
        let mut out = b"ctorch._utils\n_rebuild_tensor_v2\n(".to_vec();
        // Persistent id: ("storage", FloatStorage, key, "cpu", numel).
        out.extend_from_slice(b"(X\x07\x00\x00\x00storagectorch\nFloatStorage\n");
        out.push(b'X');
        out.extend_from_slice(&(key.len() as u32).to_le_bytes());
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(b"X\x03\x00\x00\x00cpuK\x06t");
        out.push(b'Q');
        out.push(b'K');
        out.push(offset);
        push_int_tuple(&mut out, shape);
        push_int_tuple(&mut out, stride);
        out.push(0x89); // requires_grad = False
        out.extend_from_slice(b"ccollections\nOrderedDict\n)R");
        out.push(b't');
        out.push(b'R');
        out
    }

    fn push_int_tuple(out: &mut Vec<u8>, ints: &[u8]) {
        out.push(b'(');
        for &i in ints {
            out.push(b'K');
            out.push(i);
        }
        out.push(b't');
    }

    fn key_bytes(name: &str) -> Vec<u8> {
        let mut out = vec![0x8c, name.len() as u8];
        out.extend_from_slice(name.as_bytes());
        out
    }

    /// A state dict of `(name, pickled value)` pairs, as `torch.save` writes it.
    fn state_dict(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = b"\x80\x02ccollections\nOrderedDict\n)Rq\x00(".to_vec();
        for (name, value) in entries {
            out.extend(key_bytes(name));
            out.extend_from_slice(value);
        }
        out.push(b'u');
        out.push(b'.');
        out
    }

    /// A zip checkpoint with the given `data.pkl` and storage entries, all stored.
    fn checkpoint(pickle: &[u8], storages: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("model/data.pkl", options).unwrap();
        zip.write_all(pickle).unwrap();
        for (key, bytes) in storages {
            zip.start_file(format!("model/data/{key}"), options)
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    /// Six f32 values, 0.0 to 5.0, as little-endian bytes.
    fn storage_bytes() -> Vec<u8> {
        (0..6u32).flat_map(|i| (i as f32).to_le_bytes()).collect()
    }

    fn bytes_of(file: &[u8], tensor: &TensorInfo) -> Vec<u8> {
        file[tensor.begin as usize..tensor.end as usize].to_vec()
    }

    #[test]
    fn tensors_point_at_their_bytes_in_the_file() {
        let pickle = state_dict(&[
            ("w", tensor_pickle("0", 0, &[2, 3], &[3, 1])),
            ("b", tensor_pickle("0", 3, &[3], &[1])),
        ]);
        let file = checkpoint(&pickle, &[("0", storage_bytes())]);

        let model = parse(&mut Cursor::new(&file)).unwrap();
        let names: Vec<&str> = model.tensors.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["b", "w"]);
        assert_eq!(model.metadata["format"], "pytorch");

        let w = model.tensors.iter().find(|t| t.name == "w").unwrap();
        assert_eq!(w.dtype, "F32");
        assert_eq!(w.shape, [2, 3]);
        assert_eq!(bytes_of(&file, w), storage_bytes());

        let b = model.tensors.iter().find(|t| t.name == "b").unwrap();
        assert_eq!(bytes_of(&file, b), storage_bytes()[12..].to_vec());
    }

    #[test]
    fn non_tensor_values_are_refused() {
        let pickle = state_dict(&[
            ("w", tensor_pickle("0", 0, &[6], &[1])),
            ("epoch", b"K\x03".to_vec()),
        ]);
        let file = checkpoint(&pickle, &[("0", storage_bytes())]);
        let err = parse(&mut Cursor::new(&file)).expect_err("must be refused");
        assert!(err.to_string().contains("`epoch`"), "{err}");
    }

    #[test]
    fn transposed_views_are_refused() {
        // A 2x3 view with strides (1, 2) reads the storage column by column.
        let pickle = state_dict(&[("t", tensor_pickle("0", 0, &[2, 3], &[1, 2]))]);
        let file = checkpoint(&pickle, &[("0", storage_bytes())]);
        let err = parse(&mut Cursor::new(&file)).expect_err("must be refused");
        assert!(err.to_string().contains("not contiguous"), "{err}");
    }

    #[test]
    fn views_past_the_storage_are_refused() {
        let pickle = state_dict(&[("w", tensor_pickle("0", 4, &[4], &[1]))]);
        let file = checkpoint(&pickle, &[("0", storage_bytes())]);
        let err = parse(&mut Cursor::new(&file)).expect_err("must be refused");
        assert!(err.to_string().contains("past the end"), "{err}");
    }

    #[test]
    fn packs_and_reads_back_through_the_archive() {
        let pickle = state_dict(&[
            ("w", tensor_pickle("0", 0, &[2, 3], &[3, 1])),
            ("b", tensor_pickle("0", 3, &[3], &[1])),
        ]);
        let file = checkpoint(&pickle, &[("0", storage_bytes())]);

        let mut packed = Vec::new();
        let opts = crate::PackOptions::default();
        let stats = crate::pack(Cursor::new(&file), &mut packed, &opts).unwrap();
        assert_eq!(stats.tensors, 2);

        let mut archive = crate::Archive::open(Cursor::new(packed.clone())).unwrap();
        archive.verify().unwrap();
        assert_eq!(archive.read_tensor("w").unwrap(), storage_bytes());
        assert_eq!(archive.read_tensor("b").unwrap(), storage_bytes()[12..]);

        // Unpacking gives a safetensors file that reads back to the same tensors.
        let mut safetensors = Vec::new();
        crate::unpack(&mut archive, &mut safetensors).unwrap();
        let model = crate::safetensors::parse(&mut Cursor::new(safetensors)).unwrap();
        assert_eq!(model.tensors.len(), 2);
    }

    #[test]
    fn a_zip_without_a_pickle_is_refused() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("readme.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"hi").unwrap();
        let file = zip.finish().unwrap().into_inner();
        let err = parse(&mut Cursor::new(&file)).expect_err("must be refused");
        assert!(err.to_string().contains("data.pkl"), "{err}");
    }

    /// A legacy persistent id: the zip form, with a trailing None.
    fn legacy_tensor_pickle(key: &str, offset: u8, shape: &[u8], stride: &[u8]) -> Vec<u8> {
        let mut out = b"ctorch._utils\n_rebuild_tensor\n(".to_vec();
        out.extend_from_slice(b"(X\x07\x00\x00\x00storagectorch\nFloatStorage\n");
        out.push(b'X');
        out.extend_from_slice(&(key.len() as u32).to_le_bytes());
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(b"X\x03\x00\x00\x00cpuK\x06Nt");
        out.push(b'Q');
        out.push(b'K');
        out.push(offset);
        push_int_tuple(&mut out, shape);
        push_int_tuple(&mut out, stride);
        out.extend_from_slice(b"ccollections\nOrderedDict\n)Rt");
        out.push(b'R');
        out
    }

    /// A legacy checkpoint: magic number, protocol, system info, the object,
    /// the storage key list, then each storage as an element count and its bytes.
    fn legacy_checkpoint(
        pickle_for_key: &[u8],
        keys: &[&str],
        storages: &[(&str, Vec<u8>, u64)],
    ) -> Vec<u8> {
        let mut file = Vec::new();
        // Magic number as a 16-byte LONG1, then the protocol and system info.
        file.extend_from_slice(b"\x80\x02\x8a\x10");
        file.extend_from_slice(&[
            0x6c, 0xfc, 0x9c, 0x46, 0xf9, 0x20, 0x6a, 0xa8, 0x50, 0x19, 0x2e, 0x80, 0, 0, 0, 0,
        ]);
        file.extend_from_slice(b".");
        file.extend_from_slice(b"\x80\x02M\xe9\x03.");
        file.extend_from_slice(b"\x80\x02}q\x00.");
        file.extend_from_slice(pickle_for_key);
        // The key list, as protocol-2 strings.
        file.extend_from_slice(b"\x80\x02](");
        for key in keys {
            file.push(0x8c);
            file.push(key.len() as u8);
            file.extend_from_slice(key.as_bytes());
        }
        file.extend_from_slice(b"e.");
        for (_, bytes, numel) in storages {
            file.extend_from_slice(&numel.to_le_bytes());
            file.extend_from_slice(bytes);
        }
        file
    }

    #[test]
    fn legacy_checkpoints_are_read_by_their_storage_records() {
        let weights = storage_bytes();
        let main = {
            let mut out = b"\x80\x02ccollections\nOrderedDict\n)Rq\x00(".to_vec();
            out.extend(key_bytes("w"));
            out.extend(legacy_tensor_pickle("0", 0, &[2, 3], &[3, 1]));
            out.extend(key_bytes("b"));
            out.extend(legacy_tensor_pickle("0", 3, &[3], &[1]));
            out.extend_from_slice(b"u.");
            out
        };
        let file = legacy_checkpoint(&main, &["0"], &[("0", weights.clone(), 6)]);

        let model = parse_legacy(&mut Cursor::new(&file)).unwrap();
        assert_eq!(model.metadata["format"], "pytorch-legacy");
        let w = model.tensors.iter().find(|t| t.name == "w").unwrap();
        assert_eq!(bytes_of(&file, w), weights);
        let b = model.tensors.iter().find(|t| t.name == "b").unwrap();
        assert_eq!(bytes_of(&file, b), weights[12..].to_vec());
    }

    #[test]
    fn legacy_storage_counts_must_match_the_pickle() {
        let main = {
            let mut out = b"\x80\x02ccollections\nOrderedDict\n)Rq\x00(".to_vec();
            out.extend(key_bytes("w"));
            out.extend(legacy_tensor_pickle("0", 0, &[6], &[1]));
            out.extend_from_slice(b"u.");
            out
        };
        // The file says 5 elements, but the pickle's storage holds 6.
        let file = legacy_checkpoint(&main, &["0"], &[("0", storage_bytes(), 5)]);
        let err = parse_legacy(&mut Cursor::new(&file)).expect_err("must be refused");
        assert!(err.to_string().contains("wrong element count"), "{err}");
    }
}
