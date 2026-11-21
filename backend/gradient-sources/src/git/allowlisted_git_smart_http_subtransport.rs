/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::git_credentials::{
    CURRENT_THREAD_GIT_ALLOWLIST_REFUSAL_TEXT, CURRENT_THREAD_GIT_CREDENTIALS, HttpsGitCredential,
};
use super::git_network_limits::{
    GitNetworkStreamReadDeadlineAndByteLimitChecker, RefAdvertisementEnd,
    blocking_git_fetch_limit_exceeded_reason_before_connect,
};
use super::guarded_git_network_connections::block_on_git_network_runtime_from_blocking_thread;
use bytes::Bytes;
use git2::transport::{Service, SmartSubtransport, SmartSubtransportStream, Transport};
use gradient_types::https_git_url_host::https_host_from_repository_url;
use gradient_util::http::{HttpClientForUserSuppliedUrls, HttpUpstreamCredential, user_agent};
use gradient_util::http_validation::{DestinationUrlRejectedError, HostHasNoAllowedAddressError};
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use reqwest::{Method, StatusCode};
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, PoisonError};
use url::Url;

const MAX_BUFFERED_REQUEST_BODY_BYTES: usize = 32 * 1024 * 1024;

pub(super) fn create_smart_transport_for_http_or_https_remote(
    remote: &git2::Remote<'_>,
) -> Result<Transport, git2::Error> {
    let subtransport = CURRENT_THREAD_GIT_CREDENTIALS.with_borrow(|credentials| {
        GitHttpSmartSubtransport::new(
            credentials.http_client().clone(),
            credentials.https_git_credentials.clone(),
        )
    });
    Transport::smart(remote, true, subtransport)
}

pub(super) fn find_https_credential_matching_url_host<'a>(
    credentials: &'a [HttpsGitCredential],
    url: &str,
) -> Option<&'a HttpsGitCredential> {
    let host = https_host_from_repository_url(url)?;
    credentials
        .iter()
        .find(|credential| credential.host.eq_ignore_ascii_case(&host))
}

pub(in crate::git) struct GitHttpSmartSubtransport {
    client: HttpClientForUserSuppliedUrls,
    https_credentials: Vec<HttpsGitCredential>,
    base_url_after_redirect: Arc<Mutex<Option<Url>>>,
}

impl GitHttpSmartSubtransport {
    pub(in crate::git) fn new(
        client: HttpClientForUserSuppliedUrls,
        https_credentials: Vec<HttpsGitCredential>,
    ) -> Self {
        Self {
            client,
            https_credentials,
            base_url_after_redirect: Arc::new(Mutex::new(None)),
        }
    }

    pub(in crate::git) fn request_stream_for_service(
        &self,
        url: &str,
        service: Service,
    ) -> Result<GitSmartHttpRequestAndResponseStream, git2::Error> {
        if let Some(reason) = blocking_git_fetch_limit_exceeded_reason_before_connect() {
            return Err(git2::Error::from_str(reason));
        }
        let invalid_url = |error: url::ParseError| {
            git2::Error::from_str(&format!("Invalid URL for the Git server: {error}"))
        };
        let mut remote_url = Url::parse(url).map_err(invalid_url)?;
        let user_info_credential = basic_credential_from_url_user_info(&remote_url);
        let _ = remote_url.set_username("");
        let _ = remote_url.set_password(None);

        let (program, listing) = match service {
            Service::UploadPackLs => ("git-upload-pack", true),
            Service::ReceivePackLs => ("git-receive-pack", true),
            Service::UploadPack => ("git-upload-pack", false),
            Service::ReceivePack => ("git-receive-pack", false),
        };
        let base = match listing {
            true => remote_url.clone(),
            false => self
                .base_url_after_redirect
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .unwrap_or_else(|| remote_url.clone()),
        };
        let base = base.as_str().trim_end_matches('/');
        let request_url = match listing {
            true => format!("{base}/info/refs?service={program}"),
            false => format!("{base}/{program}"),
        };
        let request_url = Url::parse(&request_url).map_err(invalid_url)?;
        let credential = self.basic_credential_and_host_for_request_url(
            &request_url,
            &remote_url,
            user_info_credential,
        );

        Ok(GitSmartHttpRequestAndResponseStream {
            client: self.client.clone(),
            method: if listing { Method::GET } else { Method::POST },
            url: request_url,
            request_content_type: (!listing).then(|| format!("application/x-{program}-request")),
            response_content_type: match listing {
                true => format!("application/x-{program}-advertisement"),
                false => format!("application/x-{program}-result"),
            },
            credential,
            base_url_after_redirect: listing.then(|| Arc::clone(&self.base_url_after_redirect)),
            request_body: Vec::new(),
            request_body_byte_limit: MAX_BUFFERED_REQUEST_BODY_BYTES,
            response: None,
            unread_response_bytes: Bytes::new(),
        })
    }

    fn basic_credential_and_host_for_request_url(
        &self,
        request_url: &Url,
        remote_url: &Url,
        user_info_credential: Option<HttpUpstreamCredential>,
    ) -> Option<(HttpUpstreamCredential, String)> {
        if let Some(stored) =
            find_https_credential_matching_url_host(&self.https_credentials, request_url.as_str())
        {
            let credential = HttpUpstreamCredential::Basic {
                user: stored.username.clone(),
                password: stored.password.clone(),
            };
            return Some((credential, stored.host.clone()));
        }
        let credential =
            user_info_credential.filter(|_| request_url.origin() == remote_url.origin())?;
        let host = request_url.host_str()?;
        let host = match request_url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        Some((credential, host))
    }
}

impl SmartSubtransport for GitHttpSmartSubtransport {
    fn action(
        &self,
        url: &str,
        action: Service,
    ) -> Result<Box<dyn SmartSubtransportStream>, git2::Error> {
        let stream = self.request_stream_for_service(url, action)?;
        let end = match action {
            Service::UploadPackLs | Service::ReceivePackLs => {
                RefAdvertisementEnd::RefAdvertisementIsWholeStream
            }
            Service::UploadPack | Service::ReceivePack => {
                RefAdvertisementEnd::NoRefAdvertisementInStream
            }
        };
        Ok(Box::new(
            GitNetworkStreamReadDeadlineAndByteLimitChecker::new(stream, end),
        ))
    }

    fn close(&self) -> Result<(), git2::Error> {
        Ok(())
    }
}

/// libgit2 can write the whole request body before any response read. The code can collect that body in memory and send the body on the first read. Gradient can send the pack for a force push of the update branch for the flake lock as its largest body. That pack is one commit with the changed lock file.
pub(in crate::git) struct GitSmartHttpRequestAndResponseStream {
    client: HttpClientForUserSuppliedUrls,
    method: Method,
    url: Url,
    request_content_type: Option<String>,
    response_content_type: String,
    credential: Option<(HttpUpstreamCredential, String)>,
    base_url_after_redirect: Option<Arc<Mutex<Option<Url>>>>,
    request_body: Vec<u8>,
    pub(in crate::git) request_body_byte_limit: usize,
    response: Option<reqwest::Response>,
    unread_response_bytes: Bytes,
}

impl GitSmartHttpRequestAndResponseStream {
    pub(in crate::git) fn request_builder(
        &self,
    ) -> Result<reqwest::RequestBuilder, DestinationUrlRejectedError> {
        let request = self
            .client
            .build_allowed_git_server_request(
                self.method.clone(),
                self.url.as_str(),
                self.credential.as_ref().map(|(credential, _)| credential),
            )?
            .header(USER_AGENT, format!("git/2.0 ({})", user_agent()));
        Ok(match &self.request_content_type {
            Some(content_type) => request
                .header(CONTENT_TYPE, content_type)
                .header(ACCEPT, &self.response_content_type),
            None => request,
        })
    }

    fn send_request(&mut self) -> io::Result<reqwest::Response> {
        if let Some(reason) = blocking_git_fetch_limit_exceeded_reason_before_connect() {
            return Err(io::Error::other(reason));
        }
        let mut request = self.request_builder().map_err(|refusal| {
            io::Error::other(
                store_current_thread_git_allowlist_refusal_text_and_return_it(refusal.to_string()),
            )
        })?;
        if self.request_content_type.is_some() {
            request = request.body(std::mem::take(&mut self.request_body));
        }
        let response =
            block_on_git_network_runtime_from_blocking_thread(async move { request.send().await })
                .map_err(|error| {
                    io::Error::other(
                        error_text_for_libgit2_from_reqwest_error_or_allowlist_refusal(error),
                    )
                })?;

        match (response.status(), &self.credential) {
            (StatusCode::UNAUTHORIZED, Some((_, host))) => {
                return Err(io::Error::other(format!(
                    "authentication failed: the server for {host} rejected the HTTPS credentials"
                )));
            }
            (StatusCode::UNAUTHORIZED, None) => {
                return Err(io::Error::other(
                    "remote authentication required but no callback set",
                ));
            }
            (status, _) if !status.is_success() => {
                return Err(io::Error::other(format!(
                    "unexpected http status code: {}",
                    status.as_u16()
                )));
            }
            _ => {}
        }
        if let Some(base_url_after_redirect) = &self.base_url_after_redirect {
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .map(str::trim);
            if content_type != Some(self.response_content_type.as_str()) {
                return Err(io::Error::other(
                    "The server doesn't support smart HTTP for Git. The Content-Type in the response to the ref listing is incorrect.",
                ));
            }
            let mut base = response.url().clone();
            base.set_query(None);
            let path = base.path().trim_end_matches("/info/refs").to_owned();
            base.set_path(&path);
            *base_url_after_redirect
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(base);
        }
        Ok(response)
    }
}

impl Read for GitSmartHttpRequestAndResponseStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.unread_response_bytes.is_empty() {
            let mut response = match self.response.take() {
                Some(response) => response,
                None => self.send_request()?,
            };
            let chunk =
                block_on_git_network_runtime_from_blocking_thread(async { response.chunk().await })
                    .map_err(|error| io::Error::other(error.without_url()))?;
            self.response = Some(response);
            match chunk {
                Some(chunk) => self.unread_response_bytes = chunk,
                None => return Ok(0),
            }
        }
        let count = buf.len().min(self.unread_response_bytes.len());
        buf[..count].copy_from_slice(&self.unread_response_bytes.split_to(count));
        Ok(count)
    }
}

impl Write for GitSmartHttpRequestAndResponseStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.request_body.len() + buf.len() > self.request_body_byte_limit {
            return Err(io::Error::other(format!(
                "The request body to the Git server is larger than {} bytes. Gradient held the whole request in memory before sending. The Git operation stopped here.",
                self.request_body_byte_limit
            )));
        }
        self.request_body.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn error_text_for_libgit2_from_reqwest_error_or_allowlist_refusal(error: reqwest::Error) -> String {
    let mut cause_texts = Vec::new();
    for cause in std::iter::successors(std::error::Error::source(&error), |cause| cause.source()) {
        if cause.is::<DestinationUrlRejectedError>() || cause.is::<HostHasNoAllowedAddressError>() {
            return store_current_thread_git_allowlist_refusal_text_and_return_it(
                cause.to_string(),
            );
        }
        cause_texts.push(cause.to_string());
    }
    cause_texts.dedup();
    std::iter::once(error.without_url().to_string())
        .chain(cause_texts)
        .collect::<Vec<_>>()
        .join(": ")
}

fn store_current_thread_git_allowlist_refusal_text_and_return_it(text: String) -> String {
    CURRENT_THREAD_GIT_ALLOWLIST_REFUSAL_TEXT.set(Some(text.clone()));
    text
}

fn basic_credential_from_url_user_info(url: &Url) -> Option<HttpUpstreamCredential> {
    if url.username().is_empty() {
        return None;
    }
    Some(HttpUpstreamCredential::Basic {
        user: decode_percent_encoded_url_user_or_password(url.username()),
        password: decode_percent_encoded_url_user_or_password(url.password().unwrap_or_default())
            .into(),
    })
}

fn decode_percent_encoded_url_user_or_password(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let escaped = bytes
            .get(index + 1..index + 3)
            .filter(|hex| byte == b'%' && hex.iter().all(u8::is_ascii_hexdigit))
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match escaped {
            Some(escaped) => {
                decoded.push(escaped);
                index += 3;
            }
            None => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
