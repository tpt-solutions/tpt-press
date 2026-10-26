//! X25519 keys for public-key encryption. A [`Recipient`] public key seals an
//! archive, and the matching [`Identity`] opens it. Both are written as bech32m
//! strings, so a mistyped key fails its checksum instead of being used.

use std::fmt;
use std::str::FromStr;

use bech32::{Bech32m, Hrp};
use rand_core::OsRng;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::error::{Error, Result};

/// Human-readable part of a public key string, `tptpub1...`.
const PUBLIC_HRP: &str = "tptpub";
/// Human-readable part of an identity string, `TPTSEC1...`.
const SECRET_HRP: &str = "tptsec";

/// A public key that archives can be encrypted to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recipient(PublicKey);

/// The secret half of a [`Recipient`]. Opens archives encrypted to its public key.
#[derive(Clone)]
pub struct Identity(StaticSecret);

impl Identity {
    /// Generates a new identity from the operating system's randomness.
    pub fn generate() -> Self {
        Self(StaticSecret::random_from_rng(OsRng))
    }

    /// The public key to give to senders.
    pub fn to_public(&self) -> Recipient {
        Recipient(PublicKey::from(&self.0))
    }

    pub(crate) fn secret(&self) -> &StaticSecret {
        &self.0
    }
}

impl Recipient {
    pub(crate) fn key(&self) -> &PublicKey {
        &self.0
    }
}

impl FromStr for Recipient {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(Self(PublicKey::from(decode_key(s, PUBLIC_HRP)?)))
    }
}

impl fmt::Display for Recipient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hrp = Hrp::parse(PUBLIC_HRP).expect("PUBLIC_HRP is a valid human-readable part");
        let text = bech32::encode::<Bech32m>(hrp, self.0.as_bytes()).map_err(|_| fmt::Error)?;
        f.write_str(&text)
    }
}

impl FromStr for Identity {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(Self(StaticSecret::from(decode_key(s, SECRET_HRP)?)))
    }
}

impl fmt::Display for Identity {
    /// Uppercase, as is conventional for secret keys. Parsing accepts either case.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hrp = Hrp::parse(SECRET_HRP).expect("SECRET_HRP is a valid human-readable part");
        let secret = self.0.to_bytes();
        let text = bech32::encode_upper::<Bech32m>(hrp, &secret).map_err(|_| fmt::Error)?;
        f.write_str(&text)
    }
}

/// Decodes a 32-byte key whose human-readable part must be `expected_hrp`.
fn decode_key(s: &str, expected_hrp: &str) -> Result<[u8; 32]> {
    let (hrp, data) = bech32::decode(s).map_err(|e| Error::InvalidKey(e.to_string()))?;
    // bech32 keeps the case the string was written in, so compare without it.
    if !hrp.as_str().eq_ignore_ascii_case(expected_hrp) {
        return Err(Error::InvalidKey(format!(
            "expected a `{expected_hrp}1` key"
        )));
    }
    <[u8; 32]>::try_from(data.as_slice())
        .map_err(|_| Error::InvalidKey("key has the wrong length".to_owned()))
}
