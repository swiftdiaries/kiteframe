use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use clap::{Args, Parser, Subcommand};
use kiteframe_contract::{
    CapabilityLock, ComponentMetadataCatalog, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, FeatureSet, PackageIdentity, PackagePath, ResolvedAgent,
    RuntimeTargetDescriptor,
};
use kiteframe_core::{
    AgentPackage, PackageLimits, canonical_json, hash_domain, load_package, load_runtime_binding,
};
use kiteframe_resolver::{
    CandidatePolicy, ResolutionInput, lock_package, resolve_agent, validate_catalog, verify_lock,
    write_lock_atomic,
};

use crate::render::{
    CheckResult, ExplainCapability, ExplainChild, ExplainFeatures, ExplainResult, InvalidResult,
    LockResult, feature_names, write_human_diagnostics, write_human_explain, write_human_status,
    write_json, write_json_to_path,
};

const TARGET_DIGEST_DOMAIN: &[u8] = b"runtime-target-catalog";

#[derive(Debug, Parser)]
#[command(
    name = "kiteframe",
    version,
    about = "Validate and resolve Kiteframe packages"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Check(CheckArgs),
    Lock(LockArgs),
    Explain(ExplainArgs),
    Compile(CompileArgs),
}

#[derive(Debug, Args)]
struct CheckArgs {
    package: PathBuf,
    #[arg(long)]
    locked: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct LockArgs {
    package: PathBuf,
    #[arg(long, value_name = "CANONICAL_JSON")]
    catalog: PathBuf,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ExplainArgs {
    #[command(flatten)]
    resolution: ResolutionArgs,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct CompileArgs {
    #[command(flatten)]
    resolution: ResolutionArgs,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ResolutionArgs {
    package: PathBuf,
    #[arg(long)]
    binding: PathBuf,
    #[arg(long)]
    target: PathBuf,
    #[arg(long, required = true, action = clap::ArgAction::SetTrue)]
    locked: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ExitCategory {
    Success = 0,
    Package = 2,
    LockOrCatalog = 3,
    Resolution = 4,
    RuntimeTarget = 5,
}

impl ExitCategory {
    pub(crate) const fn code(self) -> i32 {
        self as i32
    }
}

pub(crate) fn run(cli: Cli) -> ExitCategory {
    match cli.command {
        Command::Check(args) => run_check(args),
        Command::Lock(args) => run_lock(args),
        Command::Explain(args) => run_explain(args),
        Command::Compile(args) => run_compile(args),
    }
}

pub(crate) fn render_argument_failure() -> ExitCategory {
    render_failure(
        vec![diagnostic(
            DiagnosticCode::PackageInvalid,
            DiagnosticCategory::Package,
            DiagnosticStage::Parse,
            "command arguments are invalid",
        )],
        true,
    )
}

fn run_check(args: CheckArgs) -> ExitCategory {
    let result = load_package(&args.package, PackageLimits::V1).and_then(|package| {
        if args.locked {
            let lock = read_lock(default_lock_path(&package))?;
            verify_lock(&package, &lock, None)?;
        }
        Ok(package)
    });

    match result {
        Ok(package) => {
            let rendered = if args.json {
                write_json(&CheckResult {
                    status: "valid",
                    package_identity: &package.manifest().metadata,
                    portable_digest: package.portable_digest(),
                })
            } else {
                write_human_status(&format!(
                    "valid: {} {} {}",
                    package.manifest().metadata.name,
                    package.manifest().metadata.version,
                    package.portable_digest()
                ))
            };
            render_success(rendered)
        }
        Err(diagnostics) => render_failure(diagnostics, args.json),
    }
}

fn run_lock(args: LockArgs) -> ExitCategory {
    let result = load_package(&args.package, PackageLimits::V1).and_then(|package| {
        let catalog_bytes = read_file(
            &args.catalog,
            DiagnosticCode::CatalogIncompatible,
            DiagnosticCategory::Catalog,
            DiagnosticStage::Lock,
            "capability catalog cannot be read",
        )?;
        let catalog = validate_catalog(&catalog_bytes)?;
        let lock = lock_package(&package, &catalog, CandidatePolicy::AllowAll)?;
        let output = args
            .output
            .clone()
            .unwrap_or_else(|| default_lock_path(&package));
        write_lock_atomic(&output, &lock).map_err(|diagnostic| vec![diagnostic])?;
        Ok(lock)
    });

    match result {
        Ok(lock) => {
            let rendered = if args.json {
                write_json(&LockResult {
                    status: "locked",
                    lock_digest: &lock.lock_digest,
                    capability_count: lock.capabilities.len(),
                })
            } else {
                write_human_status(&format!("locked: {}", lock.lock_digest))
            };
            render_success(rendered)
        }
        Err(diagnostics) => render_failure(diagnostics, args.json),
    }
}

fn run_explain(args: ExplainArgs) -> ExitCategory {
    match resolve_pipeline(&args.resolution) {
        Ok(output) => {
            let explanation = explain(output);
            let rendered = if args.json {
                write_json(&explanation)
            } else {
                write_human_explain(&explanation)
            };
            render_success(rendered)
        }
        Err(diagnostics) => render_failure(diagnostics, args.json),
    }
}

fn run_compile(args: CompileArgs) -> ExitCategory {
    match resolve_pipeline(&args.resolution) {
        Ok(output) => match args.output {
            Some(path) => {
                let result =
                    validated_compile_output_path(&args.resolution, &path).and_then(|path| {
                        write_json_to_path(&path, &output.resolved)
                            .map_err(|_| compile_output_diagnostic(false))
                    });
                match result {
                    Ok(()) => ExitCategory::Success,
                    Err(diagnostic) => render_failure(vec![diagnostic], args.json),
                }
            }
            None => render_success(write_json(&output.resolved)),
        },
        Err(diagnostics) => render_failure(diagnostics, args.json),
    }
}

fn validated_compile_output_path(
    args: &ResolutionArgs,
    output: &Path,
) -> Result<PathBuf, Diagnostic> {
    let output = canonicalize_output_path(output).map_err(|_| compile_output_diagnostic(false))?;
    let package_root =
        fs::canonicalize(&args.package).map_err(|_| compile_output_diagnostic(false))?;
    let protected_inputs = [
        package_root.join("agent.yaml"),
        package_root.join("capability.lock"),
        fs::canonicalize(&args.binding).map_err(|_| compile_output_diagnostic(false))?,
        fs::canonicalize(&args.target).map_err(|_| compile_output_diagnostic(false))?,
    ];

    if output.starts_with(&package_root) || protected_inputs.contains(&output) {
        return Err(compile_output_diagnostic(true));
    }

    Ok(output)
}

fn canonicalize_output_path(path: &Path) -> std::io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let Some(file_name) = path.file_name() else {
                return Err(error);
            };
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            fs::canonicalize(parent).map(|parent| parent.join(file_name))
        }
        Err(error) => Err(error),
    }
}

fn compile_output_diagnostic(overlap: bool) -> Diagnostic {
    diagnostic(
        DiagnosticCode::CompileOutput,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Runtime,
        if overlap {
            "compiled IR output path overlaps protected input"
        } else {
            "compiled IR output cannot be written"
        },
    )
}

struct PipelineOutput {
    resolved: ResolvedAgent,
    portable_digest: kiteframe_contract::Sha256Digest,
    requested_optional_features: Vec<kiteframe_contract::Feature>,
}

fn resolve_pipeline(args: &ResolutionArgs) -> Result<PipelineOutput, Vec<Diagnostic>> {
    debug_assert!(args.locked, "clap requires locked mode for V1 resolution");
    let package = load_package(&args.package, PackageLimits::V1)?;
    let lock = read_lock(default_lock_path(&package))?;
    verify_lock(&package, &lock, None)?;
    let binding_path = package_relative_path(&package, &args.binding)?;
    let binding = load_runtime_binding(package.root().as_path(), &binding_path, PackageLimits::V1)?;
    let (target, components) = read_target_catalog(&args.target)?;
    let child_locks = read_child_locks(&package)?;
    let requested_optional_features = package
        .manifest()
        .spec
        .features
        .optional
        .iter()
        .cloned()
        .collect();
    let portable_digest = *package.portable_digest();
    let resolved = resolve_agent(ResolutionInput {
        package,
        lock,
        child_locks,
        binding,
        target,
        components,
    })?;
    Ok(PipelineOutput {
        resolved,
        portable_digest,
        requested_optional_features,
    })
}

fn explain(output: PipelineOutput) -> ExplainResult {
    let portable_digest = output.portable_digest;
    let resolved = output.resolved;
    let capabilities = resolved
        .capability_requirements()
        .iter()
        .map(|requirement| ExplainCapability {
            identity: requirement.identity.clone(),
            required: requirement.required,
        })
        .collect();
    let models = resolved
        .models()
        .iter()
        .map(|(role, model)| (role.as_str().to_owned(), model.symbol().as_str().to_owned()))
        .collect();
    let required = resolved
        .required_features()
        .iter()
        .map(ToString::to_string)
        .collect();
    let enabled_optional: Vec<_> = resolved
        .optional_features()
        .iter()
        .map(ToString::to_string)
        .collect();
    let enabled_set: BTreeSet<_> = enabled_optional.iter().cloned().collect();
    let omitted_optional = feature_names(output.requested_optional_features)
        .into_iter()
        .filter(|feature| !enabled_set.contains(feature))
        .collect();
    let child_delegation_boundaries = resolved
        .subagents()
        .iter()
        .map(|child| ExplainChild {
            package_identity: child.package_identity.clone(),
            delegated_capabilities: child
                .delegation
                .capabilities
                .iter()
                .map(|capability| capability.as_str().to_owned())
                .collect(),
            resolved_digest: child.resolved_digest,
        })
        .collect();
    let report = resolved.compilation_report();
    let mut diagnostics = report.warnings.clone();
    diagnostics
        .sort_by(|left, right| (&left.code, &left.message).cmp(&(&right.code, &right.message)));
    let mut precedence_decisions = report.decisions.clone();
    precedence_decisions.sort_by(|left, right| {
        (&left.subject, &left.outcome).cmp(&(&right.subject, &right.outcome))
    });

    ExplainResult {
        status: "resolved",
        package_identity: resolved.package_identity().clone(),
        portable_digest,
        lock_digest: resolved.lock_digest().to_owned(),
        capabilities,
        models,
        features: ExplainFeatures {
            required,
            enabled_optional,
            omitted_optional,
        },
        precedence_decisions,
        child_delegation_boundaries,
        diagnostics,
    }
}

fn read_target_catalog(
    path: &Path,
) -> Result<(RuntimeTargetDescriptor, ComponentMetadataCatalog), Vec<Diagnostic>> {
    let bytes = read_file(
        path,
        DiagnosticCode::ComponentUnresolved,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Resolve,
        "runtime target metadata cannot be read",
    )?;
    let components: ComponentMetadataCatalog = serde_json::from_slice(&bytes).map_err(|_| {
        vec![diagnostic(
            DiagnosticCode::ComponentUnresolved,
            DiagnosticCategory::Runtime,
            DiagnosticStage::Resolve,
            "runtime target metadata is invalid",
        )]
    })?;
    let canonical = canonical_json(&components).map_err(|_| {
        vec![diagnostic(
            DiagnosticCode::ComponentUnresolved,
            DiagnosticCategory::Runtime,
            DiagnosticStage::Resolve,
            "runtime target metadata cannot be canonicalized",
        )]
    })?;
    let supported_features: FeatureSet = components
        .components
        .values()
        .flat_map(|component| component.features.iter().cloned())
        .collect();
    let target = RuntimeTargetDescriptor {
        target: components.target.clone(),
        supported_features,
        target_digest: hash_domain(TARGET_DIGEST_DOMAIN, [canonical.as_slice()]),
    };
    Ok((target, components))
}

fn read_child_locks(
    package: &AgentPackage,
) -> Result<BTreeMap<PackageIdentity, CapabilityLock>, Vec<Diagnostic>> {
    let mut locks = BTreeMap::new();
    collect_child_locks(package, &mut locks)?;
    Ok(locks)
}

fn collect_child_locks(
    package: &AgentPackage,
    locks: &mut BTreeMap<PackageIdentity, CapabilityLock>,
) -> Result<(), Vec<Diagnostic>> {
    for child in package.subagents().values() {
        let lock = read_lock(default_lock_path(child))?;
        if locks
            .insert(child.manifest().metadata.clone(), lock)
            .is_some()
        {
            return Err(vec![diagnostic(
                DiagnosticCode::LockStale,
                DiagnosticCategory::Lock,
                DiagnosticStage::Resolve,
                "duplicate child lock identity",
            )]);
        }
        collect_child_locks(child, locks)?;
    }
    Ok(())
}

fn default_lock_path(package: &AgentPackage) -> PathBuf {
    package.root().as_path().join("capability.lock")
}

fn read_lock(path: PathBuf) -> Result<CapabilityLock, Vec<Diagnostic>> {
    let bytes = read_file(
        &path,
        DiagnosticCode::LockStale,
        DiagnosticCategory::Lock,
        DiagnosticStage::Lock,
        "capability lock cannot be read",
    )?;
    serde_json::from_slice(&bytes).map_err(|_| {
        vec![diagnostic(
            DiagnosticCode::LockTampered,
            DiagnosticCategory::Lock,
            DiagnosticStage::Lock,
            "capability lock is invalid",
        )]
    })
}

fn package_relative_path(
    package: &AgentPackage,
    selected: &Path,
) -> Result<PackagePath, Vec<Diagnostic>> {
    let canonical = selected.canonicalize().map_err(|_| {
        vec![diagnostic(
            DiagnosticCode::PackageInvalid,
            DiagnosticCategory::Package,
            DiagnosticStage::Validate,
            "runtime binding cannot be opened",
        )]
    })?;
    let relative = canonical
        .strip_prefix(package.root().as_path())
        .map_err(|_| {
            vec![diagnostic(
                DiagnosticCode::PackageContainment,
                DiagnosticCategory::Package,
                DiagnosticStage::Validate,
                "runtime binding escapes package root",
            )]
        })?;
    PackagePath::new(relative.to_string_lossy().replace('\\', "/")).map_err(|_| {
        vec![diagnostic(
            DiagnosticCode::PackageContainment,
            DiagnosticCategory::Package,
            DiagnosticStage::Validate,
            "runtime binding path is invalid",
        )]
    })
}

fn read_file(
    path: &Path,
    code: DiagnosticCode,
    category: DiagnosticCategory,
    stage: DiagnosticStage,
    message: &'static str,
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    fs::read(path).map_err(|_| vec![diagnostic(code, category, stage, message)])
}

fn diagnostic(
    code: DiagnosticCode,
    category: DiagnosticCategory,
    stage: DiagnosticStage,
    message: &'static str,
) -> Diagnostic {
    Diagnostic::error(code, category, stage, message)
}

fn render_failure(mut diagnostics: Vec<Diagnostic>, json: bool) -> ExitCategory {
    diagnostics.sort_by(|left, right| {
        (
            left.stage,
            left.package_path.as_deref(),
            left.source_range,
            left.code.as_str(),
            left.message.as_str(),
        )
            .cmp(&(
                right.stage,
                right.package_path.as_deref(),
                right.source_range,
                right.code.as_str(),
                right.message.as_str(),
            ))
    });
    let exit = exit_category(&diagnostics);
    let rendered = if json {
        write_json(&InvalidResult {
            status: "invalid",
            diagnostics: &diagnostics,
        })
    } else {
        write_human_diagnostics(&diagnostics)
    };
    if rendered.is_err() {
        ExitCategory::RuntimeTarget
    } else {
        exit
    }
}

fn render_success(result: std::io::Result<()>) -> ExitCategory {
    if result.is_ok() {
        ExitCategory::Success
    } else {
        ExitCategory::RuntimeTarget
    }
}

fn exit_category(diagnostics: &[Diagnostic]) -> ExitCategory {
    diagnostics
        .iter()
        .map(|diagnostic| {
            if diagnostic.category == DiagnosticCategory::Runtime {
                ExitCategory::RuntimeTarget
            } else if matches!(
                diagnostic.category,
                DiagnosticCategory::Lock | DiagnosticCategory::Catalog
            ) {
                ExitCategory::LockOrCatalog
            } else if diagnostic.stage == DiagnosticStage::Resolve
                || diagnostic.category == DiagnosticCategory::Feature
            {
                ExitCategory::Resolution
            } else {
                ExitCategory::Package
            }
        })
        .max_by_key(|category| *category as u8)
        .unwrap_or(ExitCategory::Package)
}
