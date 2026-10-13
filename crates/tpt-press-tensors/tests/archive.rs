//! Integration tests for `.tptn` tensor archives.

use std::collections::BTreeMap;
use std::io::Cursor;

use tpt_press_tensors::safetensors::{self, TensorInfo};
use tpt_press_tensors::{pack, unpack, Archive, Error, PackOptions, MAX_PIECE_BYTES};

/// One tensor to put in a safetensors file.
struct Source {
    name: &'static str,
    dtype: &'static str,
    shape: Vec<u64>,
    data: Vec<u8>,
}

/// Builds a safetensors file from the given tensors, laid out in the order given.
fn safetensors_file(tensors: &[Source], metadata: &BTreeMap<String, String>) -> Vec<u8> {
    let mut infos = Vec::new();
    let mut begin = 0u64;
    for t in tensors {
        infos.push(TensorInfo {
            name: t.name.to_owned(),
            dtype: t.dtype.to_owned(),
            shape: t.shape.clone(),
            begin,
            end: begin + t.data.len() as u64,
        });
        begin += t.data.len() as u64;
    }
    let mut out = safetensors::build_header(&infos, metadata).unwrap();
    for t in tensors {
        out.extend_from_slice(&t.data);
    }
    out
}

/// Pseudo-random bytes that zstd cannot shrink.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Little-endian f32 values that repeat, so zstd shrinks them.
fn smooth_f32(count: usize) -> Vec<u8> {
    (0..count)
        .flat_map(|i| ((i % 64) as f32 * 0.5).to_le_bytes())
        .collect()
}

fn sample_model() -> (Vec<Source>, BTreeMap<String, String>) {
    let tensors = vec![
        Source {
            name: "layer.0.weight",
            dtype: "F32",
            shape: vec![64, 32],
            data: smooth_f32(64 * 32),
        },
        Source {
            name: "layer.0.bias",
            dtype: "F16",
            shape: vec![32],
            data: noise(64, 7),
        },
        Source {
            name: "scale",
            dtype: "F64",
            shape: vec![],
            data: noise(8, 9),
        },
        Source {
            name: "empty",
            dtype: "U8",
            shape: vec![0],
            data: Vec::new(),
        },
    ];
    let mut metadata = BTreeMap::new();
    metadata.insert("format".to_owned(), "pt".to_owned());
    metadata.insert("source".to_owned(), "unit test".to_owned());
    (tensors, metadata)
}

fn pack_model(file: Vec<u8>, opts: &PackOptions) -> Vec<u8> {
    let mut out = Vec::new();
    pack(Cursor::new(file), &mut out, opts).unwrap();
    out
}

#[test]
fn every_tensor_round_trips() {
    let (tensors, metadata) = sample_model();
    let file = safetensors_file(&tensors, &metadata);
    let archive_bytes = pack_model(file, &PackOptions::default());

    let mut archive = Archive::open(Cursor::new(archive_bytes)).unwrap();
    assert_eq!(archive.metadata(), &metadata);
    for t in &tensors {
        assert_eq!(archive.read_tensor(t.name).unwrap(), t.data, "{}", t.name);
    }
    archive.verify().unwrap();
}

#[test]
fn metadata_is_available_without_reading_data() {
    let (tensors, metadata) = sample_model();
    let archive_bytes = pack_model(
        safetensors_file(&tensors, &metadata),
        &PackOptions::default(),
    );
    let archive = Archive::open(Cursor::new(archive_bytes)).unwrap();

    let weight = archive.tensor("layer.0.weight").unwrap();
    assert_eq!(weight.dtype, "F32");
    assert_eq!(weight.shape, vec![64, 32]);
    assert_eq!(weight.byte_len, 64 * 32 * 4);
    assert!(archive.tensor("missing").is_none());
    assert_eq!(archive.tensors().len(), 4);
}

#[test]
fn smooth_tensors_compress_and_noise_is_stored_raw() {
    let (tensors, metadata) = sample_model();
    let archive_bytes = pack_model(
        safetensors_file(&tensors, &metadata),
        &PackOptions::default(),
    );
    let mut archive = Archive::open(Cursor::new(archive_bytes.clone())).unwrap();

    let weight = archive
        .tensors()
        .iter()
        .find(|t| t.name == "layer.0.weight")
        .unwrap()
        .clone();
    let weight_piece = archive.read_tensor("layer.0.weight").unwrap();
    assert_eq!(weight_piece.len() as u64, weight.byte_len);
    assert!(
        (archive_bytes.len() as u64) < weight.byte_len,
        "archive should be smaller than the smooth tensor alone"
    );
}

#[test]
fn large_tensor_is_split_at_element_boundaries() {
    let count = (MAX_PIECE_BYTES as usize / 4) * 2 + 1000;
    let big = Source {
        name: "big",
        dtype: "F32",
        shape: vec![count as u64],
        data: smooth_f32(count),
    };
    let expected = big.data.clone();
    let file = safetensors_file(&[big], &BTreeMap::new());
    let mut archive =
        Archive::open(Cursor::new(pack_model(file, &PackOptions::default()))).unwrap();

    let mut pieces = Vec::new();
    archive
        .read_tensor_into("big", |piece| {
            pieces.push(piece.to_vec());
            Ok(())
        })
        .unwrap();

    assert_eq!(
        pieces.len(),
        3,
        "a tensor over 2 pieces should split into 3"
    );
    for piece in &pieces {
        assert!(piece.len() as u64 <= MAX_PIECE_BYTES);
        assert_eq!(piece.len() % 4, 0, "pieces must end on element boundaries");
    }
    assert_eq!(pieces.concat(), expected);
}

/// Stands in for a GPU: a fixed-size device buffer that each piece is copied into.
struct DeviceBuffer {
    bytes: Vec<u8>,
    written: usize,
}

impl DeviceBuffer {
    fn new(len: usize) -> Self {
        Self {
            bytes: vec![0; len],
            written: 0,
        }
    }

    fn copy_in(&mut self, piece: &[u8]) -> tpt_press_tensors::Result<()> {
        let end = self.written + piece.len();
        if end > self.bytes.len() {
            return Err(Error::Corrupt("piece overruns the device buffer"));
        }
        self.bytes[self.written..end].copy_from_slice(piece);
        self.written = end;
        Ok(())
    }
}

#[test]
fn loads_a_tensor_chunk_by_chunk_into_a_device_buffer() {
    let count = (MAX_PIECE_BYTES as usize / 4) + 4096;
    let big = Source {
        name: "weights",
        dtype: "F32",
        shape: vec![count as u64],
        data: smooth_f32(count),
    };
    let expected = big.data.clone();
    let file = safetensors_file(&[big], &BTreeMap::new());
    let mut archive =
        Archive::open(Cursor::new(pack_model(file, &PackOptions::default()))).unwrap();

    let len = archive.tensor("weights").unwrap().byte_len as usize;
    let mut device = DeviceBuffer::new(len);
    archive
        .read_tensor_into("weights", |piece| device.copy_in(piece))
        .unwrap();

    assert_eq!(device.written, len);
    assert_eq!(device.bytes, expected);
}

#[test]
fn corrupted_piece_is_reported_by_verify() {
    let (tensors, metadata) = sample_model();
    let mut archive_bytes = pack_model(
        safetensors_file(&tensors, &metadata),
        &PackOptions { level: 3 },
    );
    // The first piece starts right after the 8-byte header.
    archive_bytes[8 + 5] ^= 0x10;

    let mut archive = Archive::open(Cursor::new(archive_bytes)).unwrap();
    assert!(matches!(
        archive.verify(),
        Err(Error::ChecksumMismatch(_)) | Err(Error::Corrupt(_)) | Err(Error::Core(_))
    ));
}

#[test]
fn unknown_tensor_is_reported() {
    let (tensors, metadata) = sample_model();
    let mut archive = Archive::open(Cursor::new(pack_model(
        safetensors_file(&tensors, &metadata),
        &PackOptions::default(),
    )))
    .unwrap();
    assert!(matches!(
        archive.read_tensor("nope"),
        Err(Error::NoSuchTensor(name)) if name == "nope"
    ));
}

#[test]
fn unpacks_to_an_equivalent_safetensors_file() {
    let (tensors, metadata) = sample_model();
    let mut archive = Archive::open(Cursor::new(pack_model(
        safetensors_file(&tensors, &metadata),
        &PackOptions::default(),
    )))
    .unwrap();

    let mut out = Vec::new();
    let stats = unpack(&mut archive, &mut out).unwrap();
    assert_eq!(stats.tensors, 4);

    let model = safetensors::parse(&mut Cursor::new(out.clone())).unwrap();
    assert_eq!(model.metadata, metadata);
    for t in &tensors {
        let info = model.tensors.iter().find(|i| i.name == t.name).unwrap();
        let start = (model.data_start + info.begin) as usize;
        let end = (model.data_start + info.end) as usize;
        assert_eq!(&out[start..end], t.data.as_slice(), "{}", t.name);
        assert_eq!(info.shape, t.shape);
        assert_eq!(info.dtype, t.dtype);
    }
}

#[test]
fn packing_rejects_a_file_that_is_not_safetensors() {
    let mut out = Vec::new();
    let result = pack(
        Cursor::new(b"definitely not safetensors at all".to_vec()),
        &mut out,
        &PackOptions::default(),
    );
    assert!(matches!(result, Err(Error::InvalidModel(_))));
}

#[test]
fn packing_rejects_a_tensor_whose_size_does_not_match_its_shape() {
    let bad = Source {
        name: "w",
        dtype: "F32",
        shape: vec![4],
        data: vec![0u8; 12],
    };
    let mut out = Vec::new();
    let result = pack(
        Cursor::new(safetensors_file(&[bad], &BTreeMap::new())),
        &mut out,
        &PackOptions::default(),
    );
    assert!(matches!(result, Err(Error::InvalidModel(_))));
}

#[test]
fn opening_rejects_a_truncated_archive() {
    let (tensors, metadata) = sample_model();
    let bytes = pack_model(
        safetensors_file(&tensors, &metadata),
        &PackOptions::default(),
    );
    let cut = bytes[..bytes.len() - 5].to_vec();
    assert!(Archive::open(Cursor::new(cut)).is_err());
}

#[test]
fn opening_rejects_a_non_archive() {
    assert!(matches!(
        Archive::open(Cursor::new(
            b"hello, this is plain text, not a tensor archive".to_vec()
        )),
        Err(Error::BadMagic)
    ));
}

#[test]
fn invalid_level_is_rejected() {
    let (tensors, metadata) = sample_model();
    let mut out = Vec::new();
    let result = pack(
        Cursor::new(safetensors_file(&tensors, &metadata)),
        &mut out,
        &PackOptions { level: 0 },
    );
    assert!(matches!(result, Err(Error::Core(_))));
}
