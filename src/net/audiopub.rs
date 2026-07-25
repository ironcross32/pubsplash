//! Audio Pub web API client: login, stream key retrieval, stream lifecycle,
//! and chat. Sessions are a `token` JWT cookie set by `POST /login`.

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("invalid email or password")]
    BadCredentials,
    #[error("unexpected server response: {0}")]
    Unexpected(String),
}

#[derive(Debug, Clone)]
pub struct StreamIdentity {
    /// Audio Pub user id — also the Icecast mount name.
    pub user_id: String,
    /// The Icecast source password.
    pub stream_key: String,
}

pub struct AudioPubClient {
    http: reqwest::Client,
    /// Site base URL, no trailing slash, e.g. `https://audiopub.site`.
    base: String,
}

impl AudioPubClient {
    pub fn new(site_url: &str) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("pubsplash/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base: site_url.trim_end_matches('/').to_string(),
        })
    }

    /// Logs in. The `token` cookie is set on success.
    ///
    /// SvelteKit form actions answer differently depending on the Accept
    /// header: browsers get a native 303 redirect, everyone else gets a 200
    /// JSON envelope (`{"type":"redirect",...}` or `{"type":"failure",
    /// "status":401,...}`). Handle both.
    pub async fn login(&self, email: &str, password: &str) -> Result<(), ApiError> {
        let response = self
            .http
            .post(format!("{}/login", self.base))
            .form(&[("email", email), ("password", password)])
            .send()
            .await?;
        match action_result(response).await? {
            ActionResult::Redirect(_) => Ok(()),
            ActionResult::Failure { status: 401, .. } => Err(ApiError::BadCredentials),
            ActionResult::Failure { status, message } => Err(ApiError::Unexpected(format!(
                "login failed ({status}): {}",
                message.unwrap_or_default()
            ))),
        }
    }

    /// Fetches the user id and stream key from the stream-instructions page
    /// data endpoint (which also generates a key if the account lacks one).
    pub async fn stream_identity(&self) -> Result<StreamIdentity, ApiError> {
        let response = self
            .http
            .get(format!("{}/stream/instructions/__data.json", self.base))
            .send()
            .await?
            .error_for_status()?;
        let body: Value = response.json().await?;
        parse_stream_identity(&body)
            .ok_or_else(|| ApiError::Unexpected("no user in stream instructions data".into()))
    }

    /// Creates a stream and returns its id. If a non-finished stream already
    /// exists, the server redirects to it and we reuse that id.
    pub async fn create_stream(
        &self,
        title: &str,
        description: &str,
        archive: bool,
    ) -> Result<String, ApiError> {
        let mut form = vec![("title", title), ("description", description)];
        if archive {
            // Checkbox field: the server checks `=== "on"`; omit when unchecked.
            form.push(("shouldArchive", "on"));
        }
        let response = self
            .http
            .post(format!("{}/live/new", self.base))
            .form(&form)
            .send()
            .await?;
        match action_result(response).await? {
            ActionResult::Redirect(location) => {
                if let Some(id) = stream_id_from_location(&location) {
                    Ok(id)
                } else {
                    Err(ApiError::Unexpected(format!(
                        "create stream redirected to {location}"
                    )))
                }
            }
            ActionResult::Failure { status, message } => Err(ApiError::Unexpected(format!(
                "create stream failed ({status}): {}",
                message.unwrap_or_default()
            ))),
        }
    }

    /// Sends a chat message to the stream; returns the created message JSON.
    pub async fn send_chat(&self, stream_id: &str, content: &str) -> Result<Value, ApiError> {
        let response = self
            .http
            .post(format!("{}/live/{stream_id}", self.base))
            .json(&serde_json::json!({ "content": content }))
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json().await?)
    }

    /// Ends the stream.
    pub async fn end_stream(&self, stream_id: &str) -> Result<(), ApiError> {
        self.http
            .delete(format!("{}/live/{stream_id}", self.base))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Opens the live-events SSE response for a stream. The caller pumps the
    /// byte stream through an `SseParser`.
    pub async fn open_events(&self, stream_id: &str) -> Result<reqwest::Response, ApiError> {
        Ok(self
            .http
            .get(format!("{}/live/{stream_id}/events", self.base))
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await?
            .error_for_status()?)
    }

    #[allow(dead_code)]
    pub fn base_url(&self) -> &str {
        &self.base
    }
}

/// The outcome of a SvelteKit form action, normalized across the native
/// redirect flow and the JSON envelope flow.
enum ActionResult {
    Redirect(String),
    Failure {
        status: u16,
        message: Option<String>,
    },
}

/// Interprets a form-action response. Native flow: 3xx with a Location
/// header. JSON flow: 200 with `{"type": "redirect"|"failure"|"error", ...}`
/// where `data` is a devalue-encoded flat array.
async fn action_result(response: reqwest::Response) -> Result<ActionResult, ApiError> {
    let status = response.status();
    if status.is_redirection() {
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("/")
            .to_string();
        return Ok(ActionResult::Redirect(location));
    }
    if !status.is_success() {
        return Err(ApiError::Unexpected(format!("action returned {status}")));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|e| ApiError::Unexpected(format!("action returned non-JSON body: {e}")))?;
    parse_action_envelope(&body)
}

fn parse_action_envelope(body: &Value) -> Result<ActionResult, ApiError> {
    match body.get("type").and_then(Value::as_str) {
        Some("redirect") => Ok(ActionResult::Redirect(
            body.get("location")
                .and_then(Value::as_str)
                .unwrap_or("/")
                .to_string(),
        )),
        Some("success") => Ok(ActionResult::Redirect(String::new())),
        Some("failure") | Some("error") => {
            let status = body.get("status").and_then(Value::as_u64).unwrap_or(400) as u16;
            // `data` is a devalue string: a JSON array whose first element
            // maps field names to indices of the values.
            let message = body
                .get("data")
                .and_then(Value::as_str)
                .and_then(|data| serde_json::from_str::<Vec<Value>>(data).ok())
                .and_then(|data| {
                    let index = data.first()?.get("message")?.as_u64()? as usize;
                    Some(data.get(index)?.as_str()?.to_string())
                })
                .or_else(|| {
                    body.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
            Ok(ActionResult::Failure { status, message })
        }
        other => Err(ApiError::Unexpected(format!(
            "unrecognized action response type {other:?}"
        ))),
    }
}

fn stream_id_from_location(location: &str) -> Option<String> {
    let id = location.trim_end_matches('/').rsplit('/').next()?;
    if location.contains("/live/") && !id.is_empty() && id != "new" {
        Some(id.to_string())
    } else {
        None
    }
}

/// Resolves a value in SvelteKit's devalue flat-array encoding: objects map
/// keys to indices into the same array, so following a path means repeated
/// index lookups.
fn devalue_get<'a>(data: &'a [Value], index: usize, path: &[&str]) -> Option<&'a Value> {
    let mut current = data.get(index)?;
    for key in path {
        let idx = current.as_object()?.get(*key)?.as_u64()? as usize;
        current = data.get(idx)?;
    }
    Some(current)
}

/// Extracts `{user: {id, streamKey}}` from a `__data.json` payload.
///
/// Several nodes may carry a `user` object (the root layout returns the
/// session user without a stream key); only the stream-instructions page
/// node has both `id` and `streamKey`, so skip nodes that don't match.
fn parse_stream_identity(body: &Value) -> Option<StreamIdentity> {
    let nodes = body.get("nodes")?.as_array()?;
    nodes.iter().find_map(|node| {
        let data = node.get("data")?.as_array()?;
        let user = devalue_get(data, 0, &["user"]).filter(|u| u.is_object())?;
        // `user` is itself an object of key -> index.
        let id_idx = user.get("id")?.as_u64()? as usize;
        let key_idx = user.get("streamKey")?.as_u64()? as usize;
        Some(StreamIdentity {
            user_id: data.get(id_idx)?.as_str()?.to_string(),
            stream_key: data.get(key_idx)?.as_str()?.to_string(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devalue_stream_identity() {
        // Real shape: the layout node carries the session user (no
        // streamKey) before the page node that has both fields. The parser
        // must skip past the layout node rather than give up.
        let body: Value = serde_json::from_str(
            r#"{
                "type": "data",
                "nodes": [
                    {
                        "type": "data",
                        "data": [
                            {"user": 1, "isAdmin": 4},
                            {"id": 2, "name": 3},
                            "user-uuid-1",
                            "somebody",
                            false
                        ],
                        "uses": {}
                    },
                    {
                        "type": "data",
                        "data": [
                            {"user": 1},
                            {"id": 2, "streamKey": 3},
                            "user-uuid-1",
                            "key-uuid-9"
                        ],
                        "uses": {}
                    }
                ]
            }"#,
        )
        .unwrap();
        let identity = parse_stream_identity(&body).unwrap();
        assert_eq!(identity.user_id, "user-uuid-1");
        assert_eq!(identity.stream_key, "key-uuid-9");
    }

    /// Hits the real audiopub.site. Run explicitly with
    /// `cargo test real_login -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn real_login_bad_credentials() {
        let client = AudioPubClient::new("https://audiopub.site/").unwrap();
        match client.login("probe@example.com", "wrongpassword").await {
            Err(ApiError::BadCredentials) => {}
            other => panic!("expected BadCredentials, got {other:?}"),
        }
    }

    #[test]
    fn failure_envelope_yields_message() {
        let body: Value = serde_json::from_str(
            r#"{"type":"failure","status":401,"data":"[{\"email\":1,\"message\":2},\"probe@example.com\",\"Invalid email or password\"]"}"#,
        )
        .unwrap();
        match parse_action_envelope(&body).unwrap() {
            ActionResult::Failure { status, message } => {
                assert_eq!(status, 401);
                assert_eq!(message.as_deref(), Some("Invalid email or password"));
            }
            _ => panic!("expected failure"),
        }
    }

    #[test]
    fn redirect_envelope_yields_location() {
        let body: Value =
            serde_json::from_str(r#"{"type":"redirect","status":303,"location":"/live/abc-123"}"#)
                .unwrap();
        match parse_action_envelope(&body).unwrap() {
            ActionResult::Redirect(location) => {
                assert_eq!(
                    stream_id_from_location(&location).as_deref(),
                    Some("abc-123")
                );
            }
            _ => panic!("expected redirect"),
        }
    }

    #[test]
    fn missing_user_yields_none() {
        // Logged-out shape: `return {user: null}` — devalue may encode null
        // directly or via index; cover the direct-null variant.
        let body: Value = serde_json::from_str(
            r#"{"type":"data","nodes":[null,{"type":"data","data":[{"user":1},null],"uses":{}}]}"#,
        )
        .unwrap();
        assert!(parse_stream_identity(&body).is_none());
    }
}
