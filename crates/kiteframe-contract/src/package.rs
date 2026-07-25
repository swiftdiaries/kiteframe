use std::{borrow::Borrow, fmt, path::Path};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("package path must be a normalized relative '/'-separated path")]
pub struct InvalidPackagePath;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct PackagePath(
    #[schemars(regex(
        pattern = r"^(?!/)(?![A-Za-z]:)(?!.*(?:^|/)\.{1,2}(?:/|$))(?!.*//)(?!.*\\)(?!.*[\x00-\x1F\x7F-\x9F]).+$"
    ))]
    String,
);

impl PackagePath {
    pub fn new(path: impl Into<String>) -> Result<Self, InvalidPackagePath> {
        let path = path.into();
        let bytes = path.as_bytes();
        let has_windows_drive_prefix =
            bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        let invalid = path.is_empty()
            || path.starts_with('/')
            || path.contains('\\')
            || path.chars().any(char::is_control)
            || has_windows_drive_prefix
            || path
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..");
        if invalid {
            return Err(InvalidPackagePath);
        }
        Ok(Self(path))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_std_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl<'de> Deserialize<'de> for PackagePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

impl Borrow<str> for PackagePath {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PackagePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidatedTextAsset {
    pub path: PackagePath,
    pub text: String,
}

impl ValidatedTextAsset {
    pub fn new(path: PackagePath, text: String) -> Self {
        Self { path, text }
    }
}
