//! Encrypts an archive two ways: with a passphrase, and to an X25519 recipient.
//! Each one is opened again, and a wrong or missing key is shown to be refused.
//!
//! Run with `cargo run -p tpt-press-core --example encrypted_archive`.

use std::io::{BufReader, Cursor};

use tpt_press_core::keys::Identity;
use tpt_press_core::{archive, crypto, Error, PackOptions, UnpackOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = "user=alice action=login status=ok\n"
        .repeat(5_000)
        .into_bytes();
    let opts = PackOptions::default();

    // 1. Passphrase. The archive is wrapped in an encrypted stream, so the file starts
    //    with the encryption magic and can be recognised without the passphrase.
    let mut sealed = Vec::new();
    crypto::pack_encrypted(Cursor::new(&data), &mut sealed, &opts, "correct horse")?;
    println!(
        "passphrase: {} bytes in, {} bytes encrypted",
        data.len(),
        sealed.len()
    );
    assert!(crypto::looks_encrypted(&sealed));

    let reader = crypto::open(BufReader::new(Cursor::new(&sealed)), Some("correct horse"))?;
    let mut restored = Vec::new();
    archive::unpack(reader, &mut restored, &UnpackOptions::default())?;
    assert_eq!(restored, data);
    println!("passphrase: round trip ok");

    match crypto::open(BufReader::new(Cursor::new(&sealed)), None) {
        Err(Error::PassphraseRequired) => println!("passphrase: no passphrase given, refused"),
        Err(e) => return Err(e.into()),
        Ok(_) => return Err("opened without a passphrase".into()),
    }
    if let Err(e) = crypto::open(BufReader::new(Cursor::new(&sealed)), Some("wrong guess")) {
        println!("passphrase: wrong passphrase, refused ({e})");
    }

    // 2. Public-key recipients. Anyone with the matching identity can open the
    //    archive, and no passphrase needs to be shared.
    let identity = Identity::generate();
    let recipient = identity.to_public();
    let mut sealed = Vec::new();
    crypto::pack_for_recipients(
        Cursor::new(&data),
        &mut sealed,
        &opts,
        std::slice::from_ref(&recipient),
    )?;
    println!("recipient: sealed to {}", recipient);

    let reader = crypto::open_with_identities(
        BufReader::new(Cursor::new(&sealed)),
        std::slice::from_ref(&identity),
    )?;
    let mut restored = Vec::new();
    archive::unpack(reader, &mut restored, &UnpackOptions::default())?;
    assert_eq!(restored, data);
    println!("recipient: round trip ok");
    Ok(())
}
