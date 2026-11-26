//! What apb's MCP servers answer for the capabilities they do not
//! advertise, part of the frozen 1.x wire (`docs/MCP.md`).
//!
//! Both servers, `apb mcp` and the `apb __ask-server` sidecar, advertise
//! only `tools`. apb 1.0 answers `prompts/list`, `resources/list` and
//! `resources/templates/list` with an empty list and `completion/complete`
//! with no values, on every revision, as rmcp's default handlers did up to
//! 3.9.0. From 3.11.0 rmcp answers them `-32601` (method not found) unless
//! the capability is advertised, and advertising it would change the
//! `initialize` and `server/discover` answers instead. So each server
//! answers the four itself with the same values; rmcp still adds the
//! members a revision puts on every result (`resultType`, the cache hints).

/// The four handlers, written inside a `ServerHandler` impl.
macro_rules! answer_unadvertised {
    () => {
        async fn list_prompts(
            &self,
            _request: Option<::rmcp::model::PaginatedRequestParams>,
            _context: ::rmcp::service::RequestContext<::rmcp::RoleServer>,
        ) -> Result<::rmcp::model::ListPromptsResult, ::rmcp::ErrorData> {
            Ok(::rmcp::model::ListPromptsResult::default())
        }

        async fn list_resources(
            &self,
            _request: Option<::rmcp::model::PaginatedRequestParams>,
            _context: ::rmcp::service::RequestContext<::rmcp::RoleServer>,
        ) -> Result<::rmcp::model::ListResourcesResult, ::rmcp::ErrorData> {
            Ok(::rmcp::model::ListResourcesResult::default())
        }

        async fn list_resource_templates(
            &self,
            _request: Option<::rmcp::model::PaginatedRequestParams>,
            _context: ::rmcp::service::RequestContext<::rmcp::RoleServer>,
        ) -> Result<::rmcp::model::ListResourceTemplatesResult, ::rmcp::ErrorData> {
            Ok(::rmcp::model::ListResourceTemplatesResult::default())
        }

        async fn complete(
            &self,
            _request: ::rmcp::model::CompleteRequestParams,
            _context: ::rmcp::service::RequestContext<::rmcp::RoleServer>,
        ) -> Result<::rmcp::model::CompleteResult, ::rmcp::ErrorData> {
            Ok(::rmcp::model::CompleteResult::default())
        }
    };
}

pub(crate) use answer_unadvertised;
