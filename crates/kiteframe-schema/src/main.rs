use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use kiteframe_contract::{
    AgentManifest, CapabilityCatalog, CapabilityDescriptor, CapabilityLock,
    ComponentMetadataCatalog, ResolvedAgent, RuntimeBinding,
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
    write_schema::<ResolvedAgent>(&destination.join("resolved-agent.schema.json"))?;
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
    check_schema::<ResolvedAgent>(&destination.join("resolved-agent.schema.json"))?;
    Ok(())
}

fn usage() -> &'static str {
    "usage: kiteframe-schema [--check] <schema-directory>"
}

fn main() -> Result<()> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [destination] => generate(&PathBuf::from(destination)),
        [flag, destination] if flag == "--check" => check(&PathBuf::from(destination)),
        _ => Err(usage().into()),
    }
}
