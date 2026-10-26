//! Encryption of whole archives, by passphrase or to X25519 recipient keys.
//!
//! An encrypted archive is a normal archive wrapped in an encrypted stream. The
//! stream is split into 64 KiB chunks, each authenticated, so memory use stays
//! bounded and tampering is detected. The CRC32 checks inside the archive still
//! apply to the decrypted bytes. The primitives come from RustCrypto.
//!
//! File layout, with integers big-endian:
//!
//! ```text
//! magic     "tpt-enc/v1\n"                     11 bytes
//! count     number of key entries, 1 to 255    1 byte
//! entries   count of:
//!             passphrase: 0x01, salt[16], memory_kib u32, time u32, lanes u32, wrapped[48]
//!             recipient:  0x02, ephemeral_public[32], wrapped[48]
//! nonce     random prefix for the payload      7 bytes
//! payload   the archive in ChaCha20-Poly1305 chunks (STREAM, BE32)
//! ```
//!
//! `wrapped` is a random 32-byte file key, sealed with ChaCha20-Poly1305 under
//! a key from Argon2id (passphrase) or X25519 plus HKDF-SHA256 (recipient). The
//! nonce for it is all zeros. That is safe because each wrapping key is used once.

use std::io::{self, BufRead, Read, Write};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::stream::{
    DecryptorBE32, EncryptorBE32, Nonce as StreamNonce, StreamBE32,
};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey, SharedSecret};

use crate::archive::{self, PackOptions, Stats};
use crate::error::{Error, Result};
use crate::keys::{Identity, Recipient};

/// Magic bytes at the start of every encrypted archive. Used to detect encryption.
const MAGIC: &[u8] = b"tpt-enc/v1\n";

const PASSPHRASE_ENTRY: u8 = 1;
const RECIPIENT_ENTRY: u8 = 2;

const FILE_KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;
const WRAPPED_LEN: usize = FILE_KEY_LEN + 16;
const NONCE_PREFIX_LEN: usize = 7;
const CHUNK_SIZE: usize = 64 * 1024;
const TAG_LEN: usize = 16;

/// Memory cost used for new passphrase archives: 64 MiB, as RFC 9106 suggests.
const MEMORY_KIB: u32 = 64 * 1024;
const TIME_COST: u32 = 3;
const LANES: u32 = 4;

/// Largest Argon2 memory cost (in KiB) accepted when decrypting. 1 GiB covers
/// archives made on typical machines, and stops a hostile archive from forcing
/// a far larger allocation.
pub const MAX_MEMORY_KIB: u32 = 1024 * 1024;
/// Largest time cost accepted when decrypting.
pub const MAX_TIME_COST: u32 = 10;
/// Largest number of lanes accepted when decrypting.
pub const MAX_LANES: u32 = 16;

/// Packs `input` into an archive encrypted with `passphrase`, written to `output`.
///
/// The returned [`Stats`] describe the archive before encryption. The header and
/// the per-chunk tags are not counted.
pub fn pack_encrypted<R: Read, W: Write>(
    input: R,
    output: W,
    opts: &PackOptions,
    passphrase: &str,
) -> Result<Stats> {
    let mut writer = encrypt_with_passphrase(output, passphrase)?;
    let stats = archive::pack(input, &mut writer, opts)?;
    writer.finish()?;
    Ok(stats)
}

/// Packs `input` into an archive that any of `recipients` can open with its
/// identity. No passphrase is needed.
///
/// The returned [`Stats`] describe the archive before encryption, as in
/// [`pack_encrypted`].
pub fn pack_for_recipients<R: Read, W: Write>(
    input: R,
    output: W,
    opts: &PackOptions,
    recipients: &[Recipient],
) -> Result<Stats> {
    let mut writer = encrypt_for_recipients(output, recipients)?;
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
    let header = read_header(&mut input)?;
    let file_key = unlock_with_passphrase(&header.entries, passphrase)?;
    Ok(Box::new(Decrypted::new(
        input,
        &file_key,
        &header.nonce_prefix,
    )))
}

/// Opens an archive encrypted to recipients, using one of `identities`. An
/// unencrypted archive is returned as is, as with [`open`].
pub fn open_with_identities<'a, R: BufRead + 'a>(
    mut input: R,
    identities: &[Identity],
) -> Result<Box<dyn Read + 'a>> {
    if !is_encrypted(&mut input)? {
        return Ok(Box::new(input));
    }
    if identities.is_empty() {
        return Err(Error::InvalidArgument(
            "an identity is needed to open this archive",
        ));
    }
    let header = read_header(&mut input)?;
    let file_key = unlock_with_identities(&header.entries, identities)?;
    Ok(Box::new(Decrypted::new(
        input,
        &file_key,
        &header.nonce_prefix,
    )))
}

/// Reports whether `input` begins with the encrypted-archive magic, without
/// consuming it. An input shorter than the magic is treated as plain. Such a
/// file cannot be a valid encrypted archive, so it is rejected later.
fn is_encrypted<R: BufRead>(input: &mut R) -> io::Result<bool> {
    Ok(input.fill_buf()?.starts_with(MAGIC))
}

/// Whether `header`, the first bytes of a file, is an encrypted archive. An
/// encrypted archive can only be identified this way until it is decrypted.
pub fn looks_encrypted(header: &[u8]) -> bool {
    header.starts_with(MAGIC)
}

/// A writer that encrypts as it writes, so an archive packed through it never
/// exists unencrypted. [`EncryptingWriter::finish`] must be called, or the
/// encrypted stream is truncated and cannot be read.
pub struct EncryptingWriter<W: Write> {
    output: W,
    /// `None` only after `finish` has taken it.
    encryptor: Option<EncryptorBE32<ChaCha20Poly1305>>,
    /// Plaintext not yet sealed. Holds at most one chunk.
    buf: Vec<u8>,
}

impl<W: Write> Write for EncryptingWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        // A full chunk is sealed only once more data arrives. The last chunk is
        // then always the one `finish` seals, which is what makes truncation
        // detectable.
        if self.buf.len() == CHUNK_SIZE {
            self.seal_next()?;
        }
        let n = (CHUNK_SIZE - self.buf.len()).min(data.len());
        self.buf.extend_from_slice(&data[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

impl<W: Write> EncryptingWriter<W> {
    fn seal_next(&mut self) -> io::Result<()> {
        let encryptor = self
            .encryptor
            .as_mut()
            .ok_or_else(|| io::Error::other("encrypted stream already finished"))?;
        let sealed = encryptor
            .encrypt_next(self.buf.as_slice())
            .map_err(|_| io::Error::other("chunk encryption failed"))?;
        self.output.write_all(&sealed)?;
        self.buf.clear();
        Ok(())
    }

    /// Ends the encrypted stream, and returns the underlying writer.
    pub fn finish(mut self) -> Result<W> {
        let encryptor = self
            .encryptor
            .take()
            .ok_or_else(|| Error::Encrypt("stream already finished".to_owned()))?;
        let sealed = encryptor
            .encrypt_last(self.buf.as_slice())
            .map_err(|_| Error::Encrypt("chunk encryption failed".to_owned()))?;
        self.output.write_all(&sealed)?;
        self.output.flush()?;
        Ok(self.output)
    }
}

/// Starts an encrypted stream to `output` that opens with `passphrase`.
pub fn encrypt_with_passphrase<W: Write>(
    mut output: W,
    passphrase: &str,
) -> Result<EncryptingWriter<W>> {
    let file_key = random_bytes::<FILE_KEY_LEN>();
    let salt = random_bytes::<SALT_LEN>();
    let wrapping = derive_passphrase_key(passphrase, &salt, MEMORY_KIB, TIME_COST, LANES)?;
    let wrapped = seal_file_key(&file_key, &wrapping)?;

    output.write_all(MAGIC)?;
    output.write_all(&[1])?;
    output.write_all(&[PASSPHRASE_ENTRY])?;
    output.write_all(&salt)?;
    output.write_all(&MEMORY_KIB.to_be_bytes())?;
    output.write_all(&TIME_COST.to_be_bytes())?;
    output.write_all(&LANES.to_be_bytes())?;
    output.write_all(&wrapped)?;
    start_payload(output, &file_key)
}

/// Starts an encrypted stream to `output` that any of `recipients` can open.
pub fn encrypt_for_recipients<W: Write>(
    mut output: W,
    recipients: &[Recipient],
) -> Result<EncryptingWriter<W>> {
    if recipients.is_empty() {
        return Err(Error::InvalidArgument("at least one recipient is needed"));
    }
    let count = u8::try_from(recipients.len())
        .map_err(|_| Error::InvalidArgument("at most 255 recipients are allowed"))?;
    let file_key = random_bytes::<FILE_KEY_LEN>();

    output.write_all(MAGIC)?;
    output.write_all(&[count])?;
    for recipient in recipients {
        let ephemeral = EphemeralSecret::random_from_rng(OsRng);
        let ephemeral_public = PublicKey::from(&ephemeral);
        let shared = ephemeral.diffie_hellman(recipient.key());
        if !shared.was_contributory() {
            return Err(Error::InvalidKey("recipient key is not usable".to_owned()));
        }
        let wrapping =
            recipient_wrapping_key(&shared, ephemeral_public.as_bytes(), recipient.key())?;
        let wrapped = seal_file_key(&file_key, &wrapping)?;

        output.write_all(&[RECIPIENT_ENTRY])?;
        output.write_all(ephemeral_public.as_bytes())?;
        output.write_all(&wrapped)?;
    }
    start_payload(output, &file_key)
}

/// Writes the payload nonce prefix and returns the encrypting writer for it.
fn start_payload<W: Write>(
    mut output: W,
    file_key: &[u8; FILE_KEY_LEN],
) -> Result<EncryptingWriter<W>> {
    let nonce_prefix = random_bytes::<NONCE_PREFIX_LEN>();
    output.write_all(&nonce_prefix)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(file_key));
    let encryptor = EncryptorBE32::from_aead(
        cipher,
        StreamNonce::<ChaCha20Poly1305, StreamBE32<ChaCha20Poly1305>>::from_slice(&nonce_prefix),
    );
    Ok(EncryptingWriter {
        output,
        encryptor: Some(encryptor),
        buf: Vec::with_capacity(CHUNK_SIZE),
    })
}

/// One key entry from an encrypted archive's header.
enum Entry {
    Passphrase {
        salt: [u8; SALT_LEN],
        memory_kib: u32,
        time_cost: u32,
        lanes: u32,
        wrapped: [u8; WRAPPED_LEN],
    },
    Recipient {
        ephemeral_public: [u8; 32],
        wrapped: [u8; WRAPPED_LEN],
    },
}

struct Header {
    entries: Vec<Entry>,
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
}

/// Reads the header after the magic has been seen. Leaves `input` at the start
/// of the payload.
fn read_header<R: Read>(input: &mut R) -> Result<Header> {
    let _magic: [u8; MAGIC.len()] = read_array(input)?;
    let [count] = read_array::<1>(input)?;
    if count == 0 {
        return Err(Error::Decrypt("header has no key entries".to_owned()));
    }

    let mut entries = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let [kind] = read_array::<1>(input)?;
        let entry = match kind {
            PASSPHRASE_ENTRY => Entry::Passphrase {
                salt: read_array(input)?,
                memory_kib: u32::from_be_bytes(read_array(input)?),
                time_cost: u32::from_be_bytes(read_array(input)?),
                lanes: u32::from_be_bytes(read_array(input)?),
                wrapped: read_array(input)?,
            },
            RECIPIENT_ENTRY => Entry::Recipient {
                ephemeral_public: read_array(input)?,
                wrapped: read_array(input)?,
            },
            other => {
                return Err(Error::Decrypt(format!("unknown key entry type {other}")));
            }
        };
        entries.push(entry);
    }
    let nonce_prefix = read_array(input)?;
    Ok(Header {
        entries,
        nonce_prefix,
    })
}

/// Finds the file key using `passphrase`. Argon2 work is done only for
/// passphrase entries.
fn unlock_with_passphrase(entries: &[Entry], passphrase: &str) -> Result<[u8; FILE_KEY_LEN]> {
    let mut tried = false;
    for entry in entries {
        if let Entry::Passphrase {
            salt,
            memory_kib,
            time_cost,
            lanes,
            wrapped,
        } = entry
        {
            tried = true;
            if *memory_kib > MAX_MEMORY_KIB || *time_cost > MAX_TIME_COST || *lanes > MAX_LANES {
                return Err(Error::Decrypt(
                    "key derivation cost is above the accepted limit".to_owned(),
                ));
            }
            let wrapping =
                derive_passphrase_key(passphrase, salt, *memory_kib, *time_cost, *lanes)?;
            if let Some(file_key) = open_file_key(wrapped, &wrapping) {
                return Ok(file_key);
            }
        }
    }
    if tried {
        Err(Error::DecryptionFailed)
    } else {
        Err(Error::NotForThisKey)
    }
}

/// Finds the file key using one of `identities`.
fn unlock_with_identities(
    entries: &[Entry],
    identities: &[Identity],
) -> Result<[u8; FILE_KEY_LEN]> {
    for entry in entries {
        if let Entry::Recipient {
            ephemeral_public,
            wrapped,
        } = entry
        {
            let ephemeral_public = PublicKey::from(*ephemeral_public);
            for identity in identities {
                let shared = identity.secret().diffie_hellman(&ephemeral_public);
                if !shared.was_contributory() {
                    continue;
                }
                let public = identity.to_public();
                let wrapping =
                    recipient_wrapping_key(&shared, ephemeral_public.as_bytes(), public.key())?;
                if let Some(file_key) = open_file_key(wrapped, &wrapping) {
                    return Ok(file_key);
                }
            }
        }
    }
    Err(Error::NotForThisKey)
}

fn derive_passphrase_key(
    passphrase: &str,
    salt: &[u8],
    memory_kib: u32,
    time_cost: u32,
    lanes: u32,
) -> Result<[u8; 32]> {
    let params = Params::new(memory_kib, time_cost, lanes, Some(32))
        .map_err(|e| Error::Encrypt(e.to_string()))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| Error::Decrypt(e.to_string()))?;
    Ok(key)
}

fn recipient_wrapping_key(
    shared: &SharedSecret,
    ephemeral_public: &[u8; 32],
    recipient_public: &PublicKey,
) -> Result<[u8; 32]> {
    // Binding both public keys into the salt ties the key to this exchange.
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral_public);
    salt[32..].copy_from_slice(recipient_public.as_bytes());
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
    let mut key = [0u8; 32];
    hkdf.expand(b"tpt-press recipient key v1", &mut key)
        .map_err(|e| Error::Encrypt(e.to_string()))?;
    Ok(key)
}

/// Each wrapping key is used for exactly one file key, so a zero nonce is safe.
fn seal_file_key(file_key: &[u8; FILE_KEY_LEN], wrapping: &[u8; 32]) -> Result<[u8; WRAPPED_LEN]> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(wrapping));
    let sealed = cipher
        .encrypt(&Nonce::default(), file_key.as_slice())
        .map_err(|_| Error::Encrypt("cannot wrap file key".to_owned()))?;
    <[u8; WRAPPED_LEN]>::try_from(sealed.as_slice())
        .map_err(|_| Error::Encrypt("wrapped key has the wrong length".to_owned()))
}

/// The file key, if `wrapping` opens `wrapped`. A wrong key is not an error here,
/// since a header may hold entries for other passphrases or identities.
fn open_file_key(wrapped: &[u8; WRAPPED_LEN], wrapping: &[u8; 32]) -> Option<[u8; FILE_KEY_LEN]> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(wrapping));
    let opened = cipher.decrypt(&Nonce::default(), wrapped.as_slice()).ok()?;
    <[u8; FILE_KEY_LEN]>::try_from(opened.as_slice()).ok()
}

/// Decrypted payload. Each chunk is authenticated, and the final chunk is marked
/// as such, so a truncated or extended stream fails instead of ending early.
struct Decrypted<R: BufRead> {
    input: R,
    /// `None` once the last chunk has been opened.
    decryptor: Option<DecryptorBE32<ChaCha20Poly1305>>,
    /// Plaintext of the current chunk, and how much of it has been read.
    chunk: Vec<u8>,
    pos: usize,
    done: bool,
}

impl<R: BufRead> Decrypted<R> {
    fn new(input: R, file_key: &[u8; FILE_KEY_LEN], nonce_prefix: &[u8; NONCE_PREFIX_LEN]) -> Self {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(file_key));
        let decryptor = DecryptorBE32::from_aead(
            cipher,
            StreamNonce::<ChaCha20Poly1305, StreamBE32<ChaCha20Poly1305>>::from_slice(nonce_prefix),
        );
        Self {
            input,
            decryptor: Some(decryptor),
            chunk: Vec::new(),
            pos: 0,
            done: false,
        }
    }

    /// Opens the next ciphertext chunk. A chunk is last if it is short, or if no
    /// input follows it.
    fn refill(&mut self) -> io::Result<()> {
        let mut sealed = vec![0u8; CHUNK_SIZE + TAG_LEN];
        let n = read_up_to(&mut self.input, &mut sealed)?;
        let full = n == sealed.len();
        let last = !full || self.input.fill_buf()?.is_empty();
        let sealed = &sealed[..n];

        let damaged = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "decryption failed: the archive is damaged or was altered",
            )
        };
        self.chunk = if last {
            self.done = true;
            let decryptor = self.decryptor.take().ok_or_else(damaged)?;
            decryptor.decrypt_last(sealed).map_err(|_| damaged())?
        } else {
            let decryptor = self.decryptor.as_mut().ok_or_else(damaged)?;
            decryptor.decrypt_next(sealed).map_err(|_| damaged())?
        };
        self.pos = 0;
        Ok(())
    }
}

impl<R: BufRead> Read for Decrypted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pos == self.chunk.len() {
            if self.done {
                return Ok(0);
            }
            self.refill()?;
        }
        let n = buf.len().min(self.chunk.len() - self.pos);
        buf[..n].copy_from_slice(&self.chunk[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Reads until `buf` is full or the input ends. Returns the number of bytes read.
fn read_up_to<R: Read>(input: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Reads exactly `N` bytes. A short input means the archive was cut short.
fn read_array<const N: usize>(input: &mut impl Read) -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    input.read_exact(&mut buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Error::Truncated
        } else {
            Error::Io(e)
        }
    })?;
    Ok(buf)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    OsRng.fill_bytes(&mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_magic() {
        let mut encrypted: &[u8] = b"tpt-enc/v1\nrest";
        assert!(is_encrypted(&mut encrypted).unwrap());
        let mut plain: &[u8] = b"TPPR....";
        assert!(!is_encrypted(&mut plain).unwrap());
    }
}
