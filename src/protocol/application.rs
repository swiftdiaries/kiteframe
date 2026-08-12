use std::sync::Arc;

use crate::{CreateSession, Engine, PostgresStore};

use super::{
    ApplicationCommand, ApplicationContext, ApplicationResult, ProtocolError, command_session,
};

pub struct Application {
    store: Arc<PostgresStore>,
    engine: Arc<Engine>,
    session_retention: time::Duration,
}

impl Application {
    #[must_use]
    pub fn new(store: Arc<PostgresStore>, engine: Arc<Engine>) -> Self {
        Self {
            store,
            engine,
            session_retention: time::Duration::days(30),
        }
    }

    #[must_use]
    pub fn with_session_retention(mut self, retention: time::Duration) -> Self {
        self.session_retention = retention;
        self
    }

    pub async fn dispatch(
        &self,
        context: &ApplicationContext,
        command: ApplicationCommand,
    ) -> Result<ApplicationResult, ProtocolError> {
        match &command {
            ApplicationCommand::NewSession(request) => {
                let session = self
                    .store
                    .create_session(CreateSession {
                        principal: context.connection_principal.clone(),
                        working_directory: request.working_directory.clone(),
                        retention_until: time::OffsetDateTime::now_utc() + self.session_retention,
                    })
                    .await?;
                return Ok(ApplicationResult::Session(session));
            }
            ApplicationCommand::ResumeSession(request) => {
                let session = self
                    .store
                    .resume_session(request.session_id.clone(), &context.connection_principal)
                    .await?;
                return Ok(ApplicationResult::Session(session));
            }
            ApplicationCommand::ListSessions => {
                return Ok(ApplicationResult::Sessions(
                    self.store
                        .list_sessions(&context.connection_principal)
                        .await?,
                ));
            }
            ApplicationCommand::CloseSession(request) => {
                self.store
                    .close_session(request.session_id.clone(), &context.connection_principal)
                    .await?;
                return Ok(ApplicationResult::Closed);
            }
            _ => {}
        }

        let session_id = context
            .session_id
            .as_ref()
            .ok_or(ProtocolError::MissingSession)?;
        self.store
            .resume_session(session_id.clone(), &context.connection_principal)
            .await?;
        if let Some(command_session) = command_session(&command)
            && command_session != session_id
        {
            return Err(ProtocolError::SessionMismatch);
        }

        match command {
            ApplicationCommand::Publish(request) => {
                let catalog = self.store.current_catalog(session_id.clone()).await?;
                let workflow =
                    crate::dsl::compile(&request.source, &catalog, &crate::dsl::Limits::default())
                        .map_err(|errors| {
                            ProtocolError::Application(
                                errors
                                    .into_iter()
                                    .map(|error| error.message)
                                    .collect::<Vec<_>>()
                                    .join("; "),
                            )
                        })?;
                Ok(ApplicationResult::Published(
                    self.store
                        .publish(session_id.clone(), &workflow, &request.source)
                        .await?,
                ))
            }
            ApplicationCommand::UpdatePolicy(request) => {
                let catalog = self.store.current_catalog(session_id.clone()).await?;
                let revision = self
                    .store
                    .append_session_policy(
                        session_id.clone(),
                        request.expected_revision,
                        request.idempotency_key.as_str().to_owned(),
                        &request.policy,
                        &catalog,
                    )
                    .await?;
                Ok(ApplicationResult::PolicyUpdated { revision })
            }
            ApplicationCommand::Start(request) => {
                Ok(ApplicationResult::Run(self.store.start_run(request).await?))
            }
            ApplicationCommand::Get(request) => Ok(ApplicationResult::Run(
                self.store
                    .get_run(session_id.clone(), request.run_id)
                    .await?,
            )),
            ApplicationCommand::Answer(request) => {
                let run_id = request.run_id.clone();
                self.engine.answer_question(request).await?;
                Ok(ApplicationResult::Run(
                    self.store.get_run(session_id.clone(), run_id).await?,
                ))
            }
            ApplicationCommand::ResolvePermission(request) => {
                let run_id = request.run_id.clone();
                self.engine.resolve_permission(request).await?;
                Ok(ApplicationResult::Run(
                    self.store.get_run(session_id.clone(), run_id).await?,
                ))
            }
            ApplicationCommand::Cancel(request) => {
                let run_id = request.run_id.clone();
                self.engine.cancel_run(request).await?;
                Ok(ApplicationResult::Run(
                    self.store.get_run(session_id.clone(), run_id).await?,
                ))
            }
            ApplicationCommand::ReplayEvents(cursor) => {
                let after = u64::try_from(cursor.after).map_err(|_| {
                    ProtocolError::InvalidPayload("event cursor must not be negative".to_owned())
                })?;
                Ok(ApplicationResult::Events(
                    self.store
                        .events_after(session_id.clone(), cursor.run_id, after, cursor.limit)
                        .await?,
                ))
            }
            ApplicationCommand::NewSession(_)
            | ApplicationCommand::ResumeSession(_)
            | ApplicationCommand::ListSessions
            | ApplicationCommand::CloseSession(_) => {
                unreachable!("session lifecycle commands are handled before session binding")
            }
        }
    }
}
