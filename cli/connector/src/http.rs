use crate::ConnectorError;
use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(serde::Deserialize)]
struct Envelope<T> {
    error: bool,
    message: T,
}

pub(crate) async fn decode<T: DeserializeOwned>(res: Response) -> Result<T, ConnectorError> {
    let status = res.status();
    let bytes = res.bytes().await?;

    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(ConnectorError::Unauthorized);
    }

    if let Ok(env) = serde_json::from_slice::<Envelope<T>>(&bytes)
        && !env.error
    {
        return Ok(env.message);
    }

    if let Ok(env) = serde_json::from_slice::<Envelope<String>>(&bytes) {
        return Err(ConnectorError::Api { status, message: env.message });
    }

    Err(ConnectorError::Api {
        status,
        message: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

pub(crate) fn build_url(base: &str, path: &str) -> String {
    format!("{}/api/v1/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

#[expect(dead_code)]
pub(crate) fn json_body<B: Serialize + ?Sized>(rb: RequestBuilder, body: &B) -> RequestBuilder {
    rb.json(body)
}

pub(crate) fn request(
    http: &reqwest::Client,
    base_url: &str,
    token: Option<&str>,
    method: Method,
    endpoint: &str,
    auth_required: bool,
) -> Result<RequestBuilder, ConnectorError> {
    if auth_required && token.is_none() {
        return Err(ConnectorError::Unauthorized);
    }
    let mut rb = http.request(method, build_url(base_url, endpoint));
    if let Some(t) = token {
        rb = rb.header("Authorization", format!("Bearer {}", t));
    }
    Ok(rb)
}
