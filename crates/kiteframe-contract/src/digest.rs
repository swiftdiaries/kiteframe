use std::{borrow::Cow, fmt};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

/// A SHA-256 digest serialized as exactly 64 lowercase hexadecimal characters.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sha256Digest([u8; Self::BYTE_LENGTH]);

impl Sha256Digest {
    pub const BYTE_LENGTH: usize = 32;

    pub const fn from_bytes(bytes: [u8; Self::BYTE_LENGTH]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; Self::BYTE_LENGTH] {
        &self.0
    }

    fn from_hex(value: &str) -> Option<Self> {
        if value.len() != Self::BYTE_LENGTH * 2 {
            return None;
        }

        let mut bytes = [0_u8; Self::BYTE_LENGTH];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = hex_nibble(pair[0])?
                .checked_mul(16)?
                .checked_add(hex_nibble(pair[1])?)?;
        }
        Some(Self(bytes))
    }
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Sha256Digest")
            .field(&self.to_string())
            .finish()
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_hex(&value).ok_or_else(|| {
            D::Error::custom("SHA-256 digest must be 64 lowercase hexadecimal characters")
        })
    }
}

impl JsonSchema for Sha256Digest {
    fn schema_name() -> Cow<'static, str> {
        "Sha256Digest".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::Sha256Digest").into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{64}$"
        })
    }
}
