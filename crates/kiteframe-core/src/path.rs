use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use kiteframe_contract::{
    Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage, PackagePath,
    ValidatedTextAsset,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalPackageRoot(PathBuf);

impl CanonicalPackageRoot {
    pub(crate) fn open(path: &Path) -> Result<Self, Diagnostic> {
        let canonical = path
            .canonicalize()
            .map_err(|_| package_invalid(None, "package root cannot be opened"))?;
        if !canonical.is_dir() {
            return Err(package_invalid(None, "package root is not a directory"));
        }
        Ok(Self(canonical))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

pub(crate) struct ByteBudget {
    limit: usize,
    used: usize,
}

impl ByteBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self { limit, used: 0 }
    }

    fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.used)
    }

    fn charge(&mut self, bytes: usize) {
        self.used += bytes;
    }
}

pub(crate) struct ReferencedBytes {
    pub(crate) bytes: Vec<u8>,
    pub(crate) canonical_path: PathBuf,
}

pub(crate) fn open_referenced_text(
    root: &CanonicalPackageRoot,
    path: &PackagePath,
    max_asset_bytes: usize,
    budget: &mut ByteBudget,
) -> Result<ValidatedTextAsset, Diagnostic> {
    let referenced = open_referenced_bytes(root, path, max_asset_bytes, true, budget)?;
    let text = String::from_utf8(referenced.bytes)
        .map_err(|_| containment(path, "text asset is not UTF-8"))?;
    Ok(ValidatedTextAsset::new(path.clone(), text))
}

pub(crate) fn open_referenced_yaml(
    root: &CanonicalPackageRoot,
    path: &PackagePath,
    max_yaml_bytes: usize,
    budget: &mut ByteBudget,
) -> Result<ReferencedBytes, Diagnostic> {
    open_referenced_bytes(root, path, max_yaml_bytes, false, budget)
}

fn open_referenced_bytes(
    root: &CanonicalPackageRoot,
    path: &PackagePath,
    per_file_limit: usize,
    text_asset: bool,
    budget: &mut ByteBudget,
) -> Result<ReferencedBytes, Diagnostic> {
    reject_symlink_components(root.as_path(), path)?;
    let candidate = root.as_path().join(path.as_std_path());
    let canonical = candidate
        .canonicalize()
        .map_err(|_| package_invalid(Some(path), "referenced file cannot be opened"))?;
    if !canonical.starts_with(root.as_path()) {
        return Err(containment(path, "resolved path escapes package root"));
    }
    if !canonical.is_file() {
        return Err(package_invalid(
            Some(path),
            "referenced path is not a regular file",
        ));
    }

    let remaining = budget.remaining();
    let read_limit = per_file_limit.min(remaining);
    let read_limit_plus_one = read_limit.saturating_add(1);
    let mut file = File::open(&canonical)
        .map_err(|_| package_invalid(Some(path), "referenced file cannot be opened"))?;
    let mut bytes = Vec::with_capacity(read_limit_plus_one.min(64 * 1024));
    file.by_ref()
        .take(read_limit_plus_one as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| package_invalid(Some(path), "referenced file cannot be read"))?;

    if bytes.len() > read_limit {
        if per_file_limit <= remaining {
            let message = if text_asset {
                "text asset byte limit exceeded"
            } else {
                "YAML byte limit exceeded"
            };
            return Err(package_invalid(Some(path), message));
        }
        return Err(package_invalid(
            Some(path),
            "total referenced byte limit exceeded",
        ));
    }

    budget.charge(bytes.len());
    Ok(ReferencedBytes {
        bytes,
        canonical_path: canonical,
    })
}

fn reject_symlink_components(root: &Path, path: &PackagePath) -> Result<(), Diagnostic> {
    let mut candidate = root.to_path_buf();
    for component in path.as_std_path().components() {
        candidate.push(component);
        let metadata = fs::symlink_metadata(&candidate)
            .map_err(|_| package_invalid(Some(path), "referenced file cannot be opened"))?;
        if metadata.file_type().is_symlink() {
            return Err(containment(path, "referenced path contains a symlink"));
        }
    }
    Ok(())
}

pub(crate) fn containment(path: &PackagePath, message: &'static str) -> Diagnostic {
    let mut diagnostic = Diagnostic::error(
        DiagnosticCode::PackageContainment,
        DiagnosticCategory::Package,
        DiagnosticStage::Validate,
        message,
    );
    diagnostic.package_path = Some(path.as_str().to_owned());
    diagnostic
}

pub(crate) fn package_invalid(path: Option<&PackagePath>, message: &'static str) -> Diagnostic {
    let mut diagnostic = Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Validate,
        message,
    );
    diagnostic.package_path = path.map(|path| path.as_str().to_owned());
    diagnostic
}
