//! Keep request URLs out of errors before the RMCP worker logs them.

use std::{collections::HashMap, sync::Arc};

use futures_util::stream::BoxStream;
use http::{HeaderName, HeaderValue, StatusCode};
use rmcp::{
    model::ClientJsonRpcMessage,
    transport::streamable_http_client::{
        AuthRequiredError, InsufficientScopeError, SseError, StreamableHttpClient,
        StreamableHttpError, StreamableHttpPostResponse,
    },
};
use sse_stream::Sse;

type HttpError = StreamableHttpError<mcp_http::Error>;
type EventStream = BoxStream<'static, Result<Sse, SseError>>;

#[derive(Clone)]
pub(super) struct DiscoveryHttpClient(mcp_http::Client);

impl DiscoveryHttpClient {
    pub(super) fn new() -> Result<Self, mcp_http::Error> {
        // Match RMCP's default pooling and redirect limits. Follow only
        // redirects that preserve the method, body, and credential origin.
        mcp_http::Client::builder()
            .pool_max_idle_per_host(0)
            .redirect(mcp_http::redirect::Policy::custom(|attempt| {
                let preserves_method = matches!(
                    attempt.status(),
                    StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT
                );
                let same_origin = attempt
                    .previous()
                    .first()
                    .is_some_and(|original| original.origin() == attempt.url().origin());
                if !preserves_method || !same_origin {
                    attempt.stop()
                } else if attempt.previous().len() > 10 {
                    attempt.error("exceeded 10 redirects")
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map(Self)
    }
}

fn redact_error(error: HttpError) -> HttpError {
    match error {
        StreamableHttpError::Client(error) => StreamableHttpError::Client(error.without_url()),
        StreamableHttpError::AuthRequired(_) => {
            StreamableHttpError::AuthRequired(AuthRequiredError::new(String::new()))
        }
        StreamableHttpError::InsufficientScope(_) => {
            StreamableHttpError::InsufficientScope(InsufficientScopeError::new(String::new(), None))
        }
        StreamableHttpError::UnexpectedContentType(_) => {
            StreamableHttpError::UnexpectedContentType(None)
        }
        StreamableHttpError::UnexpectedServerResponse(_) => {
            StreamableHttpError::UnexpectedServerResponse("invalid MCP response".into())
        }
        error => error,
    }
}

impl StreamableHttpClient for DiscoveryHttpClient {
    type Error = mcp_http::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        self.0
            .post_message(uri, message, session_id, auth_token, headers)
            .await
            .map_err(redact_error)
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        limit: usize,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        self.0
            .post_message_with_max_sse_event_size(
                uri, message, session_id, auth_token, headers, limit,
            )
            .await
            .map_err(redact_error)
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<EventStream, HttpError> {
        self.0
            .get_stream(uri, session_id, last_event_id, auth_token, headers)
            .await
            .map_err(redact_error)
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        limit: usize,
    ) -> Result<EventStream, HttpError> {
        self.0
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_token,
                headers,
                limit,
            )
            .await
            .map_err(redact_error)
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_token: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        self.0
            .delete_session(uri, session_id, auth_token, headers)
            .await
            .map_err(redact_error)
    }
}
