//! Integration tests for passphrase-encrypted archives.
//!
//! Each encrypt or decrypt runs scrypt, which is slow by design, so these tests
//! keep the number of operations small.

use std::io::{BufReader, Cursor};

use tpt_press_core::{
    inspect, open_archive, open_with_identities, pack, pack_encrypted, pack_for_recipients, unpack,
    Error, PackOptions, UnpackOptions,
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

#[test]
fn recipient_keys_open_the_archive_and_nothing_else() {
    use age::x25519::Identity;

    let alice = Identity::generate();
    let bob = Identity::generate();
    let stranger = Identity::generate();
    let data = sample();

    let mut archive = Vec::new();
    pack_for_recipients(
        Cursor::new(&data),
        &mut archive,
        &small_chunks(),
        &[alice.to_public(), bob.to_public()],
    )
    .unwrap();
    assert!(!archive.windows(MARKER.len()).any(|w| w == MARKER));

    // Either recipient can open it.
    for identity in [&alice, &bob] {
        let mut restored = Vec::new();
        let reader = open_with_identities(
            BufReader::new(Cursor::new(&archive)),
            std::slice::from_ref(identity),
        )
        .unwrap();
        unpack(reader, &mut restored, &UnpackOptions::default()).unwrap();
        assert_eq!(restored, data);
    }

    // A key that is not a recipient is refused, with a message that says so.
    let err = open_with_identities(BufReader::new(Cursor::new(&archive)), &[stranger])
        .err()
        .expect("a stranger must not open the archive");
    assert!(matches!(err, Error::NotForThisKey), "{err}");

    // No key at all is refused too.
    assert!(matches!(
        open_with_identities(BufReader::new(Cursor::new(&archive)), &[]),
        Err(Error::InvalidArgument(_))
    ));

    // A passphrase cannot open a key-encrypted archive.
    assert!(matches!(
        open_archive(BufReader::new(Cursor::new(&archive)), Some(PASSPHRASE)),
        Err(Error::NotForThisKey)
    ));
}

#[test]
fn recipient_mode_needs_at_least_one_recipient() {
    let mut out = Vec::new();
    assert!(matches!(
        pack_for_recipients(Cursor::new(b"x"), &mut out, &small_chunks(), &[]),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn plain_archives_open_with_identities_unchanged() {
    let data = b"not encrypted at all".to_vec();
    let mut archive = Vec::new();
    pack(Cursor::new(&data), &mut archive, &small_chunks()).unwrap();
    let identity = age::x25519::Identity::generate();
    let mut restored = Vec::new();
    let reader = open_with_identities(BufReader::new(Cursor::new(&archive)), &[identity]).unwrap();
    unpack(reader, &mut restored, &UnpackOptions::default()).unwrap();
    assert_eq!(restored, data);
}
