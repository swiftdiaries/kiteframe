use std::sync::Arc;

use kiteframe::{
    Application, AuthenticatedConnection, Engine, EngineConfig, HarnessConfig,
    HarnessMode, InProcessToolRegistry, PersistedAuthorizer, PostgresStore, ResponsesClient,
    TransportKind, serve_ndjson,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = HarnessConfig::from_env()?;
    let store = Arc::new(PostgresStore::connect(&config.database_url).await?);
    let model = Arc::new(ResponsesClient::new(config.model)?);
    let engine = Arc::new(Engine {
        store: store.clone(),
        model,
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(PersistedAuthorizer::new(store.clone())),
        config: EngineConfig::default(),
    });
    let application = Application::new(store, engine);
    match config.mode {
        HarnessMode::Stdio => serve_ndjson(
            tokio::io::BufReader::new(tokio::io::stdin()),
            tokio::io::stdout(),
            &application,
            &AuthenticatedConnection { principal: config.principal, transport: TransportKind::Stdio },
        ).await?,
        HarnessMode::Server { .. } => return Err("remote ACP transports are not composed yet".into()),
    }
    Ok(())
}
