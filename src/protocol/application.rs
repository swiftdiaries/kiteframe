use std::sync::Arc;

use sqlx::Row;

use crate::{CreateSession, Engine, McpManager, McpServerConfig, PostgresStore, SessionId};

use super::{
    ApplicationCommand, ApplicationContext, ApplicationResult, ApplicationSession, ProtocolError,
    command_session,
};

pub struct Application {
    store: Arc<PostgresStore>,
    engine: Arc<Engine>,
    mcp: Option<Arc<McpManager>>,
    session_retention: time::Duration,
    definition_limits: crate::dsl::Limits,
}

impl Application {
    #[must_use]
    pub fn new(store: Arc<PostgresStore>, engine: Arc<Engine>) -> Self {
        Self {
            store,
            engine,
            mcp: None,
            session_retention: time::Duration::days(30),
            definition_limits: crate::dsl::Limits::default(),
        }
    }

    #[must_use]
    pub fn with_session_retention(mut self, retention: time::Duration) -> Self {
        self.session_retention = retention;
        self
    }

    #[must_use]
    pub fn with_mcp_manager(mut self, mcp: Arc<McpManager>) -> Self {
        self.mcp = Some(mcp);
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
                if let Err(error) = self
                    .attach_mcp(
                        session.id.clone(),
                        context.connection_principal.clone(),
                        request.mcp_servers.clone(),
                    )
                    .await
                {
                    self.store
                        .close_session_and_cancel_work(session.id, &context.connection_principal)
                        .await?;
                    return Err(error);
                }
                let view = self
                    .store
                    .resume_session(session.id, &context.connection_principal)
                    .await?;
                return Ok(ApplicationResult::Session(ApplicationSession {
                    view,
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
                if request.mcp_servers.is_empty() && session.view.mcp_attached {
                    return Err(ProtocolError::McpConfigurationRequired);
                }
                self.reattach_mcp(
                    session.view.id.clone(),
                    context.connection_principal.clone(),
                    request.mcp_servers.clone(),
                )
                .await?;
                let view = self
                    .store
                    .resume_session(session.view.id, &context.connection_principal)
                    .await?;
                return Ok(ApplicationResult::Session(ApplicationSession {
                    view,
                    working_directory: session.working_directory,
                }));
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
                let compilation =
                    crate::dsl::compile_report(&request.source, &catalog, &self.definition_limits)
                        .map_err(|errors| {
                            ProtocolError::Application(
                                errors
                                    .into_iter()
                                    .map(|error| error.message)
                                    .collect::<Vec<_>>()
                                    .join("; "),
                            )
                        })?;
                if request
                    .expected_kind
                    .is_some_and(|expected| expected != compilation.definition.definition_kind)
                {
                    return Err(ProtocolError::StableApplication {
                        code: "definition_kind_mismatch",
                        message: "published definition kind did not match the method".to_owned(),
                    });
                }
                for diagnostic in &compilation.diagnostics {
                    if diagnostic.code == "deprecated_workflow" {
                        tracing::warn!(
                            definition = %compilation.definition.name,
                            span_start = diagnostic.span.start,
                            span_end = diagnostic.span.end,
                            code = diagnostic.code,
                            "deprecated definition syntax"
                        );
                    }
                }
                Ok(ApplicationResult::Published(
                    self.store
                        .publish(session_id.clone(), &compilation.definition, &request.source)
                        .await?,
                ))
            }
            ApplicationCommand::SubmitTask(request) => self
                .store
                .submit_user_task(&context.connection_principal, request)
                .await
                .map(ApplicationResult::Task)
                .map_err(ProtocolError::from_task_store),
            ApplicationCommand::GetTask(request) => self
                .store
                .get_user_task(request.session_id, request.task_id)
                .await
                .map(ApplicationResult::Task)
                .map_err(ProtocolError::from_task_store),
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
            ApplicationCommand::RetryRecovery(request) => {
                let run_id = request.run_id.clone();
                self.engine.retry_recovery(request).await?;
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

    async fn attach_mcp(
        &self,
        session: SessionId,
        principal: crate::PrincipalId,
        configs: Vec<McpServerConfig>,
    ) -> Result<(), ProtocolError> {
        if configs.is_empty() {
            return Ok(());
        }
        self.mcp
            .as_ref()
            .ok_or(ProtocolError::McpUnavailable)?
            .attach(session, principal, configs)
            .await
            .map_err(|error| ProtocolError::Application(error.to_string()))?;
        Ok(())
    }

    async fn reattach_mcp(
        &self,
        session: SessionId,
        principal: crate::PrincipalId,
        configs: Vec<McpServerConfig>,
    ) -> Result<(), ProtocolError> {
        if configs.is_empty() {
            return Ok(());
        }
        self.mcp
            .as_ref()
            .ok_or(ProtocolError::McpUnavailable)?
            .reattach(session, principal, configs)
            .await
            .map_err(|error| ProtocolError::Application(error.to_string()))?;
        Ok(())
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
