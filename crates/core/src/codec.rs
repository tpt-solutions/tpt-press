//! Safe wrapper around zstd, pre-configured for tpt-press.

use crate::error::{Error, Result};

/// Default zstd level: the best speed/ratio tradeoff for general data.
pub const DEFAULT_LEVEL: i32 = 3;

/// Lowest level zstd accepts.
pub const MIN_LEVEL: i32 = 1;

/// Highest level zstd accepts without the `ultra` feature.
pub const MAX_LEVEL: i32 = 22;

/// Compresses `input` as a single zstd frame at `level`.
///
/// Returns [`Error::InvalidArgument`] if `level` is outside
/// [`MIN_LEVEL`]..=[`MAX_LEVEL`].
pub fn compress(input: &[u8], level: i32) -> Result<Vec<u8>> {
    validate_level(level)?;
    zstd::bulk::compress(input, level).map_err(Error::Zstd)
}

/// Checks that `level` is within [`MIN_LEVEL`]..=[`MAX_LEVEL`].
pub fn validate_level(level: i32) -> Result<()> {
    if (MIN_LEVEL..=MAX_LEVEL).contains(&level) {
        Ok(())
    } else {
        Err(Error::InvalidArgument("zstd level out of range"))
    }
}

/// Decompresses a single zstd frame.
///
/// `max_output` caps the decompressed size. Input that would expand beyond
/// it is rejected with [`Error::OutputTooLarge`] instead of allocating, which
/// guards against decompression bombs in untrusted archives.
pub fn decompress(input: &[u8], max_output: usize) -> Result<Vec<u8>> {
    zstd::bulk::decompress(input, max_output).map_err(|e| map_too_large(e, max_output))
}

/// Trains a zstd dictionary of at most `max_size` bytes from sample records.
///
/// Dictionaries help most when many small records share structure, such as
/// repeated JSON keys. zstd can refuse to train on too little data, which is
/// returned as [`Error::Zstd`].
pub fn train_dictionary<S: AsRef<[u8]>>(samples: &[S], max_size: usize) -> Result<Vec<u8>> {
    zstd::dict::from_samples(samples, max_size).map_err(Error::Zstd)
}

/// Compresses `input` as a single zstd frame, using `dictionary`, at `level`.
pub fn compress_with_dict(input: &[u8], level: i32, dictionary: &[u8]) -> Result<Vec<u8>> {
    validate_level(level)?;
    let mut compressor = zstd::bulk::Compressor::with_dictionary(level, dictionary)?;
    compressor.compress(input).map_err(Error::Zstd)
}

/// Decompresses a zstd frame made with `dictionary`. `max_output` caps the
/// output as in [`decompress`].
pub fn decompress_with_dict(input: &[u8], max_output: usize, dictionary: &[u8]) -> Result<Vec<u8>> {
    let mut decompressor = zstd::bulk::Decompressor::with_dictionary(dictionary)?;
    decompressor
        .decompress(input, max_output)
        .map_err(|e| map_too_large(e, max_output))
}

fn map_too_large(e: std::io::Error, max_output: usize) -> Error {
    if e.to_string().contains("Destination buffer is too small") {
        Error::OutputTooLarge { limit: max_output }
    } else {
        Error::Zstd(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_round_trip_and_gain() {
        let samples: Vec<Vec<u8>> = (0..500)
            .map(|i| {
                format!(r#"{{"timestamp":"2026-10-13T12:00:{:02}Z","level":"info","msg":"request {i}"}}"#, i % 60)
                    .into_bytes()
            })
            .collect();
        let dict = train_dictionary(&samples, 16 * 1024).unwrap();

        let record = &samples[7];
        let with = compress_with_dict(record, DEFAULT_LEVEL, &dict).unwrap();
        let without = compress(record, DEFAULT_LEVEL).unwrap();
        assert!(with.len() < without.len());
        assert_eq!(
            decompress_with_dict(&with, record.len(), &dict).unwrap(),
            *record
        );
    }

    #[test]
    fn dictionary_decompress_rejects_wrong_dictionary() {
        let samples: Vec<Vec<u8>> = (0..200)
            .map(|i| format!("alpha {i} beta").into_bytes())
            .collect();
        let dict = train_dictionary(&samples, 4096).unwrap();
        let other: Vec<Vec<u8>> = (0..200)
            .map(|i| format!("gamma {i} delta").into_bytes())
            .collect();
        let other_dict = train_dictionary(&other, 4096).unwrap();

        let packed = compress_with_dict(&samples[3], DEFAULT_LEVEL, &dict).unwrap();
        assert!(decompress_with_dict(&packed, 1024, &other_dict).is_err());
    }

    #[test]
    fn round_trip() {
        let data = b"tpt-press round trip ".repeat(100);
        let packed = compress(&data, DEFAULT_LEVEL).unwrap();
        assert!(packed.len() < data.len());
        assert_eq!(decompress(&packed, data.len()).unwrap(), data);
    }

    #[test]
    fn empty_input_round_trips() {
        let packed = compress(&[], DEFAULT_LEVEL).unwrap();
        assert!(decompress(&packed, 0).unwrap().is_empty());
    }

    #[test]
    fn rejects_out_of_range_level() {
        assert!(matches!(compress(b"x", 0), Err(Error::InvalidArgument(_))));
        assert!(matches!(compress(b"x", 23), Err(Error::InvalidArgument(_))));
    }

    #[test]
    fn rejects_output_over_limit() {
        let data = vec![0u8; 10_000];
        let packed = compress(&data, DEFAULT_LEVEL).unwrap();
        assert!(matches!(
            decompress(&packed, 100),
            Err(Error::OutputTooLarge { limit: 100 })
        ));
    }

    #[test]
    fn rejects_garbage_input() {
        assert!(matches!(
            decompress(b"not a zstd frame", 1024),
            Err(Error::Zstd(_))
        ));
    }
}
