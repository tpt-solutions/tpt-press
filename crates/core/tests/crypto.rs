//! Integration tests for passphrase-encrypted archives.
//!
//! Each encrypt or decrypt runs scrypt, which is slow by design, so these tests
//! keep the number of operations small.

use std::io::{BufReader, Cursor};

use tpt_press_core::{
    inspect, open_archive, pack, pack_encrypted, unpack, Error, PackOptions, UnpackOptions,
};

const PASSPHRASE: &str = "correct horse battery staple";
const MARKER: &[u8] = b"PLAINTEXT-MARKER-SHOULD-NOT-APPEAR";

fn sample() -> Vec<u8> {
    let mut data = MARKER.repeat(40);
    data.extend((0..200_000usize).map(|i| ((i * 31) % 251) as u8));
    data
}

fn small_chunks() -> PackOptions {
    PackOptions {
        chunk_size: 16 * 1024,
        ..PackOptions::default()
    }
}

fn encrypt(data: &[u8], passphrase: &str) -> Vec<u8> {
    let mut out = Vec::new();
    pack_encrypted(Cursor::new(data), &mut out, &small_chunks(), passphrase).unwrap();
    out
}

fn decrypt(archive: &[u8], passphrase: Option<&str>) -> tpt_press_core::Result<Vec<u8>> {
    let reader = open_archive(BufReader::new(Cursor::new(archive)), passphrase)?;
    let mut out = Vec::new();
    unpack(reader, &mut out, &UnpackOptions::default())?;
    Ok(out)
}

#[test]
fn encrypted_archive_round_trips() {
    let data = sample();
    let encrypted = encrypt(&data, PASSPHRASE);
    assert_eq!(decrypt(&encrypted, Some(PASSPHRASE)).unwrap(), data);
}

#[test]
fn ciphertext_hides_plaintext() {
    let encrypted = encrypt(&sample(), PASSPHRASE);
    let found = encrypted.windows(MARKER.len()).any(|w| w == MARKER);
    assert!(!found, "plaintext marker visible in encrypted archive");
}

#[test]
fn wrong_passphrase_is_rejected() {
    let encrypted = encrypt(&sample(), PASSPHRASE);
    assert!(matches!(
        decrypt(&encrypted, Some("wrong passphrase")),
        Err(Error::DecryptionFailed)
    ));
}

#[test]
fn encrypted_archive_needs_a_passphrase() {
    let encrypted = encrypt(&sample(), PASSPHRASE);
    assert!(matches!(
        decrypt(&encrypted, None),
        Err(Error::PassphraseRequired)
    ));
}

#[test]
fn tampered_ciphertext_is_rejected() {
    let mut encrypted = encrypt(&sample(), PASSPHRASE);
    let middle = encrypted.len() / 2;
    encrypted[middle] ^= 0x01;
    match decrypt(&encrypted, Some(PASSPHRASE)) {
        Err(Error::Io(e)) => assert!(e.to_string().contains("damaged"), "{e}"),
        other => panic!("expected an io error, got {other:?}"),
    }
}

#[test]
fn truncated_encrypted_archive_is_rejected() {
    let encrypted = encrypt(&sample(), PASSPHRASE);
    let cut = &encrypted[..encrypted.len() - 100];
    assert!(decrypt(cut, Some(PASSPHRASE)).is_err());
}

#[test]
fn plain_archive_opens_without_a_passphrase() {
    let data = sample();
    let mut plain = Vec::new();
    pack(Cursor::new(&data), &mut plain, &small_chunks()).unwrap();

    assert_eq!(decrypt(&plain, None).unwrap(), data);
    // A passphrase given for a plain archive is ignored.
    assert_eq!(decrypt(&plain, Some(PASSPHRASE)).unwrap(), data);
}

#[test]
fn inspect_works_through_decryption() {
    let data = sample();
    let encrypted = encrypt(&data, PASSPHRASE);

    let reader = open_archive(BufReader::new(Cursor::new(&encrypted)), Some(PASSPHRASE)).unwrap();
    let inspection = inspect(reader).unwrap();
    assert_eq!(inspection.stats.uncompressed_bytes, data.len() as u64);
}
