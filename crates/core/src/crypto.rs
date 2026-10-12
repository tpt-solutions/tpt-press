//! Passphrase encryption of whole archives, using the `age` format.
//!
//! An encrypted archive is a normal archive wrapped in an age stream. age
//! encrypts in 64 KiB authenticated chunks, so memory use stays bounded and
//! tampering is detected. The CRC32 checks inside the archive still apply to
//! the decrypted bytes. Nothing here implements cryptography directly.

use std::io::{self, BufRead, Read, Write};

use age::secrecy::SecretString;

use crate::archive::{self, PackOptions, Stats};
use crate::error::{Error, Result};

/// Magic bytes at the start of every age file. Used to detect encryption.
const AGE_MAGIC: &[u8] = b"age-encryption.org/v1\n";

/// Largest scrypt work factor (log2 N) accepted when decrypting.
///
/// The work factor sets how much memory key derivation uses. Capping it stops a
/// hostile archive from forcing gigabytes of allocation. 20 allows about 1 GiB
/// and covers archives made on typical machines.
pub const MAX_WORK_FACTOR: u8 = 20;

/// Packs `input` into an archive encrypted with `passphrase`, written to `output`.
///
/// The returned [`Stats`] describe the archive before encryption. age adds a
/// small fixed header plus a tag per 64 KiB, which these numbers do not count.
pub fn pack_encrypted<R: Read, W: Write>(
    input: R,
    output: W,
    opts: &PackOptions,
    passphrase: &str,
) -> Result<Stats> {
    let encryptor = age::Encryptor::with_user_passphrase(secret(passphrase));
    let mut writer = encryptor.wrap_output(output)?;
    let stats = archive::pack(input, &mut writer, opts)?;
    writer.finish()?;
    Ok(stats)
}

/// Opens an archive for reading. Returns a plain reader for an unencrypted
/// archive. For an encrypted one, `passphrase` is required and the returned
/// reader yields the decrypted archive bytes.
///
/// Use the returned reader with [`archive::unpack`] or [`archive::inspect`].
pub fn open<'a, R: BufRead + 'a>(
    mut input: R,
    passphrase: Option<&str>,
) -> Result<Box<dyn Read + 'a>> {
    if !is_encrypted(&mut input)? {
        return Ok(Box::new(input));
    }

    let passphrase = passphrase.ok_or(Error::PassphraseRequired)?;
    let mut identity = age::scrypt::Identity::new(secret(passphrase));
    identity.set_max_work_factor(MAX_WORK_FACTOR);

    let decryptor = age::Decryptor::new_buffered(input).map_err(map_decrypt_error)?;
    let reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(map_decrypt_error)?;
    Ok(Box::new(Decrypted(reader)))
}

/// Decrypted stream. age reports a failed chunk as a generic decryption error,
/// so this swaps it for a message that says what to check.
struct Decrypted<R: Read>(age::stream::StreamReader<R>);

impl<R: Read> Read for Decrypted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf).map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidData {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "decryption failed: the archive is damaged or was altered",
                )
            } else {
                e
            }
        })
    }
}

/// Reports whether `input` begins with the age header, without consuming it.
///
/// An input shorter than the magic is treated as plain. Such a file cannot be a
/// valid encrypted archive, so it is rejected later as a bad archive.
fn is_encrypted<R: BufRead>(input: &mut R) -> io::Result<bool> {
    Ok(input.fill_buf()?.starts_with(AGE_MAGIC))
}

fn secret(passphrase: &str) -> SecretString {
    SecretString::from(passphrase.to_owned())
}

fn map_decrypt_error(e: age::DecryptError) -> Error {
    match e {
        age::DecryptError::DecryptionFailed => Error::DecryptionFailed,
        other => Error::Decrypt(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_age_magic() {
        let mut encrypted: &[u8] = b"age-encryption.org/v1\nrest";
        assert!(is_encrypted(&mut encrypted).unwrap());
        let mut plain: &[u8] = b"TPPR....";
        assert!(!is_encrypted(&mut plain).unwrap());
    }
}
