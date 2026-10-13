//! Round trips, slice reads, filters and refusals for arrays archives.

use std::io::Cursor;
use std::ops::Range;

use proptest::prelude::*;
use tpt_press_arrays::{
    inspect, pack_npy, pack_raw, read_slice, unpack, Error, Filter, FilterChoice, PackOptions,
};

/// Builds a `.npy` version 1.0 file, the way NumPy writes one.
fn npy_file(descr: &str, shape: &[u64], data: &[u8]) -> Vec<u8> {
    let dims: Vec<String> = shape.iter().map(u64::to_string).collect();
    let tuple = if dims.len() == 1 {
        format!("({},)", dims[0])
    } else {
        format!("({})", dims.join(", "))
    };
    let mut text = format!("{{'descr': '{descr}', 'fortran_order': False, 'shape': {tuple}, }}");
    while !(10 + text.len() + 1).is_multiple_of(64) {
        text.push(' ');
    }
    text.push('\n');
    let mut out = b"\x93NUMPY\x01\x00".to_vec();
    out.extend_from_slice(&(text.len() as u16).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(data);
    out
}

fn pack_raw_bytes(data: &[u8], descr: &str, shape: &[u64], chunk: Option<Vec<u64>>) -> Vec<u8> {
    let mut archive = Vec::new();
    let options = PackOptions {
        chunk_shape: chunk,
        ..PackOptions::default()
    };
    pack_raw(
        Cursor::new(data.to_vec()),
        &mut archive,
        descr,
        shape,
        &options,
    )
    .expect("pack");
    archive
}

fn unpack_bytes(archive: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    unpack(Cursor::new(archive.to_vec()), &mut out)?;
    Ok(out)
}

fn slice_bytes(archive: &[u8], ranges: &[Range<u64>]) -> Result<Vec<u8>, Error> {
    Ok(read_slice(Cursor::new(archive.to_vec()), ranges)?.bytes)
}

/// The values 0, 1, 2, ... as little-endian bytes of `elem` size, so each element is recognisable.
fn counting(elements: u64, elem: usize) -> Vec<u8> {
    (0..elements)
        .flat_map(|i: u64| i.to_le_bytes()[..elem].to_vec())
        .collect()
}

/// The slice a naive loop over every index would give.
fn naive_slice(data: &[u8], shape: &[u64], elem: usize, ranges: &[Range<u64>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut index: Vec<u64> = ranges.iter().map(|r| r.start).collect();
    let count: u64 = ranges.iter().map(|r| r.end - r.start).product();
    for _ in 0..count {
        let mut linear = 0u64;
        for (d, &i) in index.iter().enumerate() {
            linear = linear * shape[d] + i;
        }
        let at = linear as usize * elem;
        out.extend_from_slice(&data[at..at + elem]);
        for d in (0..index.len()).rev() {
            index[d] += 1;
            if index[d] < ranges[d].end {
                break;
            }
            index[d] = ranges[d].start;
        }
    }
    out
}

#[test]
fn npy_file_round_trips_byte_for_byte() {
    let data = counting(3 * 5, 4);
    let file = npy_file("<i4", &[3, 5], &data);
    let mut archive = Vec::new();
    let stats = pack_npy(
        Cursor::new(file.clone()),
        &mut archive,
        &PackOptions::default(),
    )
    .unwrap();
    assert_eq!(stats.shape, vec![3, 5]);
    assert_eq!(stats.dtype, "<i4");
    assert_eq!(unpack_bytes(&archive).unwrap(), file);
    assert!(inspect(Cursor::new(archive)).unwrap().npy);
}

#[test]
fn raw_bytes_round_trip_for_each_element_size() {
    for (descr, elem) in [("|u1", 1), ("<i2", 2), ("<f4", 4), ("<f8", 8)] {
        let shape = [4u64, 6, 3];
        let data = counting(shape.iter().product(), elem);
        let archive = pack_raw_bytes(&data, descr, &shape, Some(vec![3, 4, 2]));
        assert_eq!(unpack_bytes(&archive).unwrap(), data, "{descr}");
    }
}

#[test]
fn edge_chunks_round_trip_when_the_shape_does_not_divide() {
    let shape = [5u64, 7];
    let data = counting(35, 2);
    let archive = pack_raw_bytes(&data, "<u2", &shape, Some(vec![2, 3]));
    assert_eq!(unpack_bytes(&archive).unwrap(), data);
    let summary = inspect(Cursor::new(archive)).unwrap();
    assert_eq!(summary.chunks, 9);
}

#[test]
fn slice_reads_only_the_chunks_it_overlaps() {
    let shape = [100u64];
    let data = counting(100, 1);
    let archive = pack_raw_bytes(&data, "|u1", &shape, Some(vec![10]));
    let slice = read_slice(Cursor::new(archive), std::slice::from_ref(&(2..5))).unwrap();
    assert_eq!(slice.bytes, vec![2, 3, 4]);
    assert_eq!(slice.shape, vec![3]);
    assert_eq!(slice.chunks_read, 1);
}

#[test]
fn smooth_floats_pick_a_filter_that_beats_none() {
    let values: Vec<f32> = (0..65_536).map(|i| 1.0 + (i as f32) * 1e-6).collect();
    let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut archive = Vec::new();
    let stats = pack_raw(
        Cursor::new(data.clone()),
        &mut archive,
        "<f4",
        &[65_536],
        &PackOptions::default(),
    )
    .unwrap();
    assert!(stats.filter_counts[Filter::None.code() as usize] < stats.chunks);
    assert_eq!(unpack_bytes(&archive).unwrap(), data);
}

#[test]
fn a_fixed_filter_is_used_for_every_chunk() {
    let data = counting(256, 8);
    let mut archive = Vec::new();
    let options = PackOptions {
        chunk_shape: Some(vec![64]),
        filter: FilterChoice::Fixed(Filter::Shuffle),
        ..PackOptions::default()
    };
    let stats = pack_raw(
        Cursor::new(data.clone()),
        &mut archive,
        "<u8",
        &[256],
        &options,
    )
    .unwrap();
    assert_eq!(stats.filter_counts, [0, 4, 0, 0, 0, 0, 0]);
    assert_eq!(unpack_bytes(&archive).unwrap(), data);
}

#[test]
fn refuses_what_it_cannot_store() {
    let mut out = Vec::new();
    let opts = PackOptions::default();
    // Data length does not match the shape.
    assert!(pack_raw(Cursor::new(vec![0u8; 5]), &mut out, "<f4", &[2], &opts).is_err());
    // Unknown element type, and big-endian data.
    assert!(matches!(
        pack_raw(Cursor::new(vec![0u8; 8]), &mut out, "<U4", &[2], &opts),
        Err(Error::InvalidDtype(_))
    ));
    assert!(matches!(
        pack_raw(Cursor::new(vec![0u8; 8]), &mut out, ">f4", &[2], &opts),
        Err(Error::Unsupported { .. })
    ));
    // Scalars, and a chunk shape with the wrong number of axes.
    assert!(pack_raw(Cursor::new(vec![0u8; 4]), &mut out, "<f4", &[], &opts).is_err());
    let bad_chunk = PackOptions {
        chunk_shape: Some(vec![2]),
        ..PackOptions::default()
    };
    assert!(pack_raw(
        Cursor::new(vec![0u8; 8]),
        &mut out,
        "<f4",
        &[1, 2],
        &bad_chunk
    )
    .is_err());
    // Fortran-ordered .npy.
    let mut fortran = npy_file("<i2", &[2], &[0u8; 4]);
    let end = 10 + u16::from_le_bytes([fortran[8], fortran[9]]) as usize;
    // "False" and " True" have the same length, so the header keeps its size.
    let text = String::from_utf8(fortran[10..end].to_vec())
        .unwrap()
        .replace("False", " True");
    fortran.splice(10..end, text.bytes());
    assert!(pack_npy(Cursor::new(fortran), &mut out, &opts).is_err());
}

#[test]
fn slices_out_of_bounds_are_refused() {
    let archive = pack_raw_bytes(&counting(12, 1), "|u1", &[3, 4], None);
    let err = read_slice(Cursor::new(archive.clone()), &[0..3, 2..5]).unwrap_err();
    assert!(
        matches!(err, Error::RangeOutOfBounds { axis: 1, .. }),
        "got {err:?}"
    );
    assert!(read_slice(Cursor::new(archive), std::slice::from_ref(&(0..3))).is_err());
}

#[test]
fn a_flipped_byte_in_a_chunk_is_detected() {
    let mut archive = pack_raw_bytes(&counting(64, 1), "|u1", &[64], Some(vec![64]));
    archive[20] ^= 0x10;
    assert!(matches!(unpack_bytes(&archive), Err(Error::Corrupt(_))));
}

#[test]
fn truncated_and_foreign_files_are_refused() {
    let archive = pack_raw_bytes(&counting(64, 1), "|u1", &[64], None);
    for cut in [archive.len() - 1, archive.len() / 2, 4] {
        assert!(unpack_bytes(&archive[..cut]).is_err(), "cut at {cut}");
    }
    assert!(matches!(
        unpack_bytes(b"this is not an arrays archive, just text"),
        Err(Error::BadMagic)
    ));
}

/// Shapes of 1 to 3 axes, each 1 to 7 long, with a chunk shape of 1 to 8 on each axis.
fn array_case() -> impl Strategy<Value = (Vec<u64>, Vec<u64>, usize)> {
    prop::collection::vec(1u64..=7, 1..=3).prop_flat_map(|shape| {
        let n = shape.len();
        let chunk = prop::collection::vec(1u64..=8, n);
        let elem = prop::sample::select(vec![1usize, 2, 4, 8]);
        (Just(shape), chunk, elem)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn any_box_matches_a_naive_read(
        (shape, chunk, elem) in array_case(),
        seed in any::<u64>(),
    ) {
        let elements: u64 = shape.iter().product();
        // Pseudo-random bytes, so filters are exercised on noisy data as well as smooth.
        let mut state = seed | 1;
        let data: Vec<u8> = (0..elements as usize * elem)
            .map(|_| {
                state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                (state >> 56) as u8
            })
            .collect();
        let archive = {
            let mut out = Vec::new();
            let options = PackOptions { chunk_shape: Some(chunk), ..PackOptions::default() };
            let descr = if elem == 1 { "|u1".to_owned() } else { format!("<u{elem}") };
            pack_raw(Cursor::new(data.clone()), &mut out, &descr, &shape, &options).unwrap();
            out
        };
        prop_assert_eq!(unpack_bytes(&archive).unwrap(), data.clone());

        // A box chosen from the seed: start and end on every axis.
        let mut s = seed;
        let ranges: Vec<Range<u64>> = shape
            .iter()
            .map(|&len| {
                s = s.wrapping_mul(2_862_933_555_777_941_757).wrapping_add(3_037_000_493);
                let a = (s >> 33) % (len + 1);
                s = s.wrapping_mul(2_862_933_555_777_941_757).wrapping_add(3_037_000_493);
                let b = (s >> 33) % (len + 1);
                a.min(b)..a.max(b)
            })
            .collect();
        let got = slice_bytes(&archive, &ranges).unwrap();
        prop_assert_eq!(got, naive_slice(&data, &shape, elem, &ranges));
    }
}

/// A zip file holding the given members, stored without compression.
fn zip_of(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in members {
        zip.start_file(*name, options).unwrap();
        std::io::Write::write_all(&mut zip, data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn a_single_array_npz_packs_as_that_array() {
    let file = npy_file("<i2", &[3, 4], &counting(12, 2));
    let npz = zip_of(&[("temps.npy", &file)]);
    assert!(tpt_press_arrays::is_npz(&npz));
    let mut archive = Vec::new();
    tpt_press_arrays::pack_npz(Cursor::new(npz), &mut archive, &PackOptions::default()).unwrap();
    assert_eq!(unpack_bytes(&archive).unwrap(), file);
}

#[test]
fn an_npz_with_several_arrays_becomes_a_bundle() {
    let a = npy_file("<u1", &[2], &[1, 2]);
    let b = npy_file("<i2", &[3], &[0, 0, 5, 0, 6, 0]);
    let npz = zip_of(&[("a.npy", &a), ("b.npy", &b)]);
    let mut file = Vec::new();
    let stats =
        tpt_press_arrays::pack_npz(Cursor::new(npz), &mut file, &PackOptions::default()).unwrap();
    assert_eq!(
        stats.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(tpt_press_arrays::is_bundle(&file));
    let names: Vec<String> = tpt_press_arrays::bundle_members(Cursor::new(&file))
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["a", "b"]);
    let member = tpt_press_arrays::read_member(Cursor::new(&file), "b").unwrap();
    assert_eq!(unpack_bytes(&member).unwrap(), b);
}
