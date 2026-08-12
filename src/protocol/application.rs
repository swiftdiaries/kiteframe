use std::sync::Arc;

use crate::{Engine, PostgresStore};

use super::{ApplicationCommand, ApplicationContext, ApplicationResult, ProtocolError, command_session};

pub struct Application {
    store: Arc<PostgresStore>,
    engine: Arc<Engine>,
}

impl Application {
    #[must_use]
    pub fn new(store: Arc<PostgresStore>, engine: Arc<Engine>) -> Self { Self { store, engine } }

    pub async fn dispatch(&self, context: &ApplicationContext, command: ApplicationCommand) -> Result<ApplicationResult, ProtocolError> {
        if matches!(command, ApplicationCommand::ListSessions) {
            return Ok(ApplicationResult::Sessions(self.store.list_sessions(&context.principal).await?));
        }
        let session_id = context.session_id.as_ref().ok_or(ProtocolError::MissingSession)?;
        self.store.resume_session(session_id.clone(), &context.principal).await?;
        if let Some(command_session) = command_session(&command) && command_session != session_id {
            return Err(ProtocolError::SessionMismatch);
        }
        match command {
            ApplicationCommand::ListSessions => unreachable!("handled before session requirement"),
            ApplicationCommand::Start(request) => Ok(ApplicationResult::Run(self.store.start_run(request).await?)),
            ApplicationCommand::Get(request) => Ok(ApplicationResult::Run(self.store.get_run(session_id.clone(), request.run_id).await?)),
            ApplicationCommand::Answer(request) => {
                let run_id = request.run_id.clone();
                self.engine.answer_question(request).await?;
                Ok(ApplicationResult::Run(self.store.get_run(session_id.clone(), run_id).await?))
            }
            ApplicationCommand::ResolvePermission(request) => {
                let run_id = request.run_id.clone();
                self.engine.resolve_permission(request).await?;
                Ok(ApplicationResult::Run(self.store.get_run(session_id.clone(), run_id).await?))
            }
            ApplicationCommand::Cancel(request) => {
                let run_id = request.run_id.clone();
                self.engine.cancel_run(request).await?;
                Ok(ApplicationResult::Run(self.store.get_run(session_id.clone(), run_id).await?))
            }
            ApplicationCommand::ReplayEvents(cursor) => Ok(ApplicationResult::Events(self.store.events_after(session_id.clone(), cursor.run_id, cursor.after, cursor.limit).await?)),
        }
    }
}
