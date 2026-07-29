use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use kiteframe_contract::{
    AdmissionRequest, AgentManifest, AuthorityRevisionSet, CapabilityCatalog, CapabilityDescriptor,
    CapabilityGrantSet, CapabilityLock, CatalogRequest, ComponentMetadataCatalog, Diagnostic,
    EffectProposal, InvocationOutcome, InvocationRequest, InvocationStatus, ResolvedAgent,
    RuntimeBinding, StatusRequest,
};
use schemars::JsonSchema;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn schema_bytes<T: JsonSchema>() -> Result<Vec<u8>> {
    let schema = schemars::schema_for!(T);
    let mut bytes = serde_json::to_vec_pretty(&schema)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_schema<T: JsonSchema>(destination: &Path) -> Result<()> {
    fs::write(destination, schema_bytes::<T>()?)?;
    Ok(())
}

fn check_schema<T: JsonSchema>(destination: &Path) -> Result<()> {
    let checked_in = fs::read(destination)?;
    let generated = schema_bytes::<T>()?;
    if checked_in != generated {
        return Err(format!("schema drift detected: {}", destination.display()).into());
    }
    Ok(())
}

fn generate(destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    write_schema::<AgentManifest>(&destination.join("agent.schema.json"))?;
    write_schema::<RuntimeBinding>(&destination.join("runtime-binding.schema.json"))?;
    write_schema::<CapabilityDescriptor>(&destination.join("capability-descriptor.schema.json"))?;
    write_schema::<CapabilityCatalog>(&destination.join("capability-catalog.schema.json"))?;
    write_schema::<CapabilityLock>(&destination.join("capability-lock.schema.json"))?;
    write_schema::<ComponentMetadataCatalog>(
        &destination.join("component-metadata-catalog.schema.json"),
    )?;
    write_schema::<Diagnostic>(&destination.join("diagnostic.schema.json"))?;
    write_schema::<ResolvedAgent>(&destination.join("resolved-agent.schema.json"))?;
    write_schema::<CatalogRequest>(&destination.join("catalog-request.schema.json"))?;
    write_schema::<AdmissionRequest>(&destination.join("admission-request.schema.json"))?;
    write_schema::<CapabilityGrantSet>(&destination.join("capability-grant-set.schema.json"))?;
    write_schema::<AuthorityRevisionSet>(&destination.join("authority-revision-set.schema.json"))?;
    write_schema::<InvocationRequest>(&destination.join("invocation-request.schema.json"))?;
    write_schema::<EffectProposal>(&destination.join("effect-proposal.schema.json"))?;
    write_schema::<StatusRequest>(&destination.join("status-request.schema.json"))?;
    write_schema::<InvocationOutcome>(&destination.join("invocation-outcome.schema.json"))?;
    write_schema::<InvocationStatus>(&destination.join("invocation-status.schema.json"))?;
    Ok(())
}

fn check(destination: &Path) -> Result<()> {
    check_schema::<AgentManifest>(&destination.join("agent.schema.json"))?;
    check_schema::<RuntimeBinding>(&destination.join("runtime-binding.schema.json"))?;
    check_schema::<CapabilityDescriptor>(&destination.join("capability-descriptor.schema.json"))?;
    check_schema::<CapabilityCatalog>(&destination.join("capability-catalog.schema.json"))?;
    check_schema::<CapabilityLock>(&destination.join("capability-lock.schema.json"))?;
    check_schema::<ComponentMetadataCatalog>(
        &destination.join("component-metadata-catalog.schema.json"),
    )?;
    check_schema::<Diagnostic>(&destination.join("diagnostic.schema.json"))?;
    check_schema::<ResolvedAgent>(&destination.join("resolved-agent.schema.json"))?;
    check_schema::<CatalogRequest>(&destination.join("catalog-request.schema.json"))?;
    check_schema::<AdmissionRequest>(&destination.join("admission-request.schema.json"))?;
    check_schema::<CapabilityGrantSet>(&destination.join("capability-grant-set.schema.json"))?;
    check_schema::<AuthorityRevisionSet>(&destination.join("authority-revision-set.schema.json"))?;
    check_schema::<InvocationRequest>(&destination.join("invocation-request.schema.json"))?;
    check_schema::<EffectProposal>(&destination.join("effect-proposal.schema.json"))?;
    check_schema::<StatusRequest>(&destination.join("status-request.schema.json"))?;
    check_schema::<InvocationOutcome>(&destination.join("invocation-outcome.schema.json"))?;
    check_schema::<InvocationStatus>(&destination.join("invocation-status.schema.json"))?;
    Ok(())
}

fn python_stub_bytes() -> Result<Vec<u8>> {
    let stub = kiteframe_native::python_stub()
        .map_err(|error| format!("Python stub generation failed: {error}"))?
        .trim_end_matches(['\r', '\n'])
        .as_bytes()
        .to_vec();
    let mut bytes = stub;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_python_stub(destination: &Path) -> Result<()> {
    fs::write(destination, python_stub_bytes()?)?;
    Ok(())
}

fn check_python_stub(destination: &Path) -> Result<()> {
    let checked_in = fs::read(destination)?;
    let generated = python_stub_bytes()?;
    if checked_in != generated {
        return Err(format!("Python stub drift detected: {}", destination.display()).into());
    }
    Ok(())
}

fn usage() -> &'static str {
    "usage: kiteframe-schema [--check] <schema-directory> | \
     --python-stubs <stub-file> | --check-python-stubs <stub-file>"
}

fn main() -> Result<()> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [destination] => generate(&PathBuf::from(destination)),
        [flag, destination] if flag == "--check" => check(&PathBuf::from(destination)),
        [flag, destination] if flag == "--python-stubs" => {
            write_python_stub(&PathBuf::from(destination))
        }
        [flag, destination] if flag == "--check-python-stubs" => {
            check_python_stub(&PathBuf::from(destination))
        }
        _ => Err(usage().into()),
    }
}
