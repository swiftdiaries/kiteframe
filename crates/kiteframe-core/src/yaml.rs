use std::{collections::HashSet, str};

use kiteframe_contract::{
    AgentManifest, Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage, RuntimeBinding,
    SourceRange,
};
use serde::de::DeserializeOwned;
use yaml_rust2::{
    parser::{Event, Parser},
    scanner::Marker,
};

/// Resource bounds applied while loading a package.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageLimits {
    pub max_yaml_bytes: usize,
    pub max_text_asset_bytes: usize,
    pub max_total_referenced_bytes: usize,
    pub max_nesting_depth: usize,
    pub max_collection_entries: usize,
    pub max_aliases: usize,
    pub max_subagent_depth: usize,
}

impl PackageLimits {
    /// Fixed limits for the V1 package format.
    pub const V1: Self = Self {
        max_yaml_bytes: 1_048_576,
        max_text_asset_bytes: 4_194_304,
        max_total_referenced_bytes: 33_554_432,
        max_nesting_depth: 32,
        max_collection_entries: 10_000,
        max_aliases: 128,
        max_subagent_depth: 16,
    };
}

/// Parse an agent manifest only after bounded event-level YAML validation.
pub fn parse_manifest(
    bytes: &[u8],
    limits: PackageLimits,
) -> Result<AgentManifest, Vec<Diagnostic>> {
    parse_typed(bytes, limits)
}

/// Parse a runtime binding only after bounded event-level YAML validation.
pub fn parse_binding(
    bytes: &[u8],
    limits: PackageLimits,
) -> Result<RuntimeBinding, Vec<Diagnostic>> {
    parse_typed(bytes, limits)
}

fn parse_typed<T: DeserializeOwned>(
    bytes: &[u8],
    limits: PackageLimits,
) -> Result<T, Vec<Diagnostic>> {
    StrictYamlScanner::new(limits).scan(bytes)?;
    serde_yaml_ng::from_slice(bytes).map_err(|error| vec![typed_yaml_diagnostic(&error)])
}

struct StrictYamlScanner {
    limits: PackageLimits,
    input_len: usize,
    frames: Vec<CollectionFrame>,
    collection_entries: usize,
    aliases: usize,
    violation: Option<Diagnostic>,
}

enum CollectionFrame {
    Sequence,
    Mapping {
        expecting_key: bool,
        scalar_keys: HashSet<String>,
    },
}

impl StrictYamlScanner {
    fn new(limits: PackageLimits) -> Self {
        Self {
            limits,
            input_len: 0,
            frames: Vec::new(),
            collection_entries: 0,
            aliases: 0,
            violation: None,
        }
    }

    fn scan(mut self, bytes: &[u8]) -> Result<(), Vec<Diagnostic>> {
        self.input_len = bytes.len();
        if bytes.len() > self.limits.max_yaml_bytes {
            let start = self.limits.max_yaml_bytes;
            return Err(vec![package_diagnostic(
                "YAML byte limit exceeded",
                Some(byte_range(start, bytes.len())),
            )]);
        }

        let source = str::from_utf8(bytes).map_err(|error| {
            vec![package_diagnostic(
                "invalid UTF-8 YAML",
                Some(byte_range(error.valid_up_to(), bytes.len())),
            )]
        })?;

        let mut parser = Parser::new_from_str(source);
        loop {
            let (event, mark) = parser.next_token().map_err(|error| {
                vec![package_diagnostic(
                    "invalid YAML syntax",
                    Some(marker_range(*error.marker(), bytes.len())),
                )]
            })?;
            let stream_ended = matches!(event, Event::StreamEnd);
            self.handle_event(event, mark);
            if let Some(violation) = self.violation {
                return Err(vec![violation]);
            }
            if stream_ended {
                return Ok(());
            }
        }
    }

    fn node_started(&mut self, scalar_key: Option<&str>, mark: Marker) {
        let mut added_entry = false;
        let mut duplicate_key = false;

        if let Some(frame) = self.frames.last_mut() {
            match frame {
                CollectionFrame::Sequence => added_entry = true,
                CollectionFrame::Mapping {
                    expecting_key,
                    scalar_keys,
                } => {
                    if *expecting_key {
                        added_entry = true;
                        if let Some(key) = scalar_key
                            && !scalar_keys.insert(key.to_owned())
                        {
                            duplicate_key = true;
                        }
                    }
                    *expecting_key = !*expecting_key;
                }
            }
        }

        if duplicate_key {
            self.reject(
                "duplicate key in YAML mapping",
                marker_range(mark, self.input_len),
            );
            return;
        }

        if added_entry {
            self.collection_entries += 1;
            if self.collection_entries > self.limits.max_collection_entries {
                self.reject(
                    "YAML collection entries limit exceeded",
                    marker_range(mark, self.input_len),
                );
            }
        }
    }

    fn collection_started(&mut self, frame: CollectionFrame, mark: Marker) {
        self.node_started(None, mark);
        if self.violation.is_some() {
            return;
        }

        self.frames.push(frame);
        if self.frames.len() > self.limits.max_nesting_depth {
            self.reject(
                "YAML nesting depth limit exceeded",
                marker_range(mark, self.input_len),
            );
        }
    }

    fn reject(&mut self, message: impl Into<String>, range: SourceRange) {
        if self.violation.is_none() {
            self.violation = Some(package_diagnostic(message, Some(range)));
        }
    }

    fn handle_event(&mut self, event: Event, mark: Marker) {
        if self.violation.is_some() {
            return;
        }

        match event {
            Event::Scalar(value, ..) => self.node_started(Some(&value), mark),
            Event::Alias(..) => {
                self.node_started(None, mark);
                self.aliases += 1;
                if self.aliases > self.limits.max_aliases {
                    self.reject(
                        "YAML alias limit exceeded",
                        marker_range(mark, self.input_len),
                    );
                }
            }
            Event::SequenceStart(..) => {
                self.collection_started(CollectionFrame::Sequence, mark);
            }
            Event::MappingStart(..) => {
                self.collection_started(
                    CollectionFrame::Mapping {
                        expecting_key: true,
                        scalar_keys: HashSet::new(),
                    },
                    mark,
                );
            }
            Event::SequenceEnd | Event::MappingEnd => {
                self.frames.pop();
            }
            _ => {}
        }
    }
}

fn package_diagnostic(message: impl Into<String>, source_range: Option<SourceRange>) -> Diagnostic {
    let mut diagnostic = Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Parse,
        message.into(),
    );
    diagnostic.source_range = source_range;
    diagnostic
}

fn typed_yaml_diagnostic(error: &serde_yaml_ng::Error) -> Diagnostic {
    let error_text = error.to_string();
    let message = if error_text.contains("unknown field") {
        "unknown field in YAML document"
    } else {
        "YAML document does not match the required schema"
    };
    package_diagnostic(message, None)
}

fn marker_range(marker: Marker, input_len: usize) -> SourceRange {
    byte_range(marker.index(), input_len)
}

fn byte_range(start: usize, input_len: usize) -> SourceRange {
    let start = start.min(input_len).min(u32::MAX as usize) as u32;
    let end = start
        .saturating_add(1)
        .min(input_len.min(u32::MAX as usize) as u32);
    SourceRange { start, end }
}
