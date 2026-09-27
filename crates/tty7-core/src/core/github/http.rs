//! [`Transport`] over HTTPS, with the same stack and proxy resolution the
//! installer and the update check use.
//!
//! Blocking — every call runs on a worker, never on the UI thread.

use std::time::Duration;

use super::api::{ApiError, Reply, Transport, classify, link_has_next};
use super::token::Token;

const API: &str = "https://api.github.com";

/// Interactive reads: long enough for a slow link, short enough that a dead
/// one is reported rather than sat on.
const TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// A pull request's file list with every patch inlined is the largest answer
/// the panel asks for; GitHub itself caps a patch well under this.
const MAX_BODY: u64 = 32 * 1024 * 1024;

pub struct HttpTransport {
    agent: ureq::Agent,
    token: Option<Token>,
}

impl HttpTransport {
    /// `manual_proxy` is the `http_proxy` setting, as for tty7's other
    /// downloads.
    pub fn new(token: Option<Token>, manual_proxy: Option<&str>) -> HttpTransport {
        let mut builder = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            // 4xx/5xx come back as responses, so their headers (the rate
            // limit's reset time) can be read.
            .http_status_as_error(false)
            .user_agent(concat!("tty7/", env!("CARGO_PKG_VERSION")));
        if let Some(proxy) = crate::daemon::install::proxy::resolve(API, manual_proxy) {
            builder = builder.proxy(Some(proxy));
        }
        HttpTransport {
            agent: builder.build().into(),
            token,
        }
    }
}

impl Transport for HttpTransport {
    fn get(&self, path: &str) -> Result<Reply, ApiError> {
        self.request(path, "application/vnd.github+json")
    }

    fn get_full(&self, path: &str) -> Result<Reply, ApiError> {
        self.request(path, "application/vnd.github.full+json")
    }

    fn authenticated(&self) -> bool {
        self.token.is_some()
    }
}

impl HttpTransport {
    fn request(&self, path: &str, accept: &str) -> Result<Reply, ApiError> {
        let mut request = self
            .agent
            .get(format!("{API}{path}"))
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            request = request.header("Authorization", format!("Bearer {}", token.expose()));
        }
        // ureq's error text names the URL and the transport failure, never
        // the request headers, so it is safe to show.
        let mut response = request
            .call()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_vec()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        classify(status, &header, &body)?;
        Ok(Reply {
            has_next: header("link").is_some_and(|l| link_has_next(&l)),
            body,
        })
    }
}
