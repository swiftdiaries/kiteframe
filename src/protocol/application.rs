use std::sync::Arc;

use sqlx::Row;

use crate::{CreateSession, Engine, PostgresStore};

use super::{
    ApplicationCommand, ApplicationContext, ApplicationResult, ApplicationSession, ProtocolError,
    command_session,
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
                return Ok(ApplicationResult::Session(ApplicationSession {
                    view: session,
                    working_directory: request.working_directory.clone(),
                }));
            }
            ApplicationCommand::ResumeSession(request) => {
                let session = self
                    .active_session(
                        request.session_id.clone(),
                        &context.connection_principal,
                        Some(&request.working_directory),
                    )
                    .await?;
                return Ok(ApplicationResult::Session(session));
            }
            ApplicationCommand::ListSessions => {
                let mut sessions = Vec::new();
                for view in self
                    .store
                    .list_sessions(&context.connection_principal)
                    .await?
                {
                    let metadata = self.session_metadata(&view.id).await?;
                    if metadata.retention_until > time::OffsetDateTime::now_utc() {
                        sessions.push(ApplicationSession {
                            view,
                            working_directory: metadata.working_directory,
                        });
                    }
                }
                return Ok(ApplicationResult::Sessions(sessions));
            }
            ApplicationCommand::CloseSession(request) => {
                self.store
                    .close_session_and_cancel_work(
                        request.session_id.clone(),
                        &context.connection_principal,
                    )
                    .await?;
                return Ok(ApplicationResult::Closed);
            }
            _ => {}
        }

        let session_id = context
            .session_id
            .as_ref()
            .ok_or(ProtocolError::MissingSession)?;
        self.active_session(session_id.clone(), &context.connection_principal, None)
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

    async fn active_session(
        &self,
        session_id: crate::SessionId,
        principal: &crate::PrincipalId,
        working_directory: Option<&str>,
    ) -> Result<ApplicationSession, ProtocolError> {
        let view = self
            .store
            .resume_session(session_id.clone(), principal)
            .await?;
        let metadata = self.session_metadata(&session_id).await?;
        if metadata.retention_until <= time::OffsetDateTime::now_utc() {
            return Err(ProtocolError::SessionExpired);
        }
        if working_directory.is_some_and(|cwd| cwd != metadata.working_directory) {
            return Err(ProtocolError::WorkingDirectoryMismatch);
        }
        Ok(ApplicationSession {
            view,
            working_directory: metadata.working_directory,
        })
    }

    async fn session_metadata(
        &self,
        session_id: &crate::SessionId,
    ) -> Result<SessionMetadata, ProtocolError> {
        let row =
            sqlx::query("SELECT working_directory, retention_until FROM sessions WHERE id = $1")
                .bind(*session_id.as_uuid())
                .fetch_optional(self.store.pool())
                .await
                .map_err(crate::StoreError::from)?
                .ok_or(crate::StoreError::SessionNotFound)?;
        Ok(SessionMetadata {
            working_directory: row.get("working_directory"),
            retention_until: row.get("retention_until"),
        })
    }
}

struct SessionMetadata {
    working_directory: String,
    retention_until: time::OffsetDateTime,
}
