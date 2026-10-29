//! The waiting page shown to browsers while a group starts.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::controller::WakeOutcome;

const TEMPLATE: &str = include_str!("assets/wait.html");

/// The logo as a `data:` URL. The page is served under the sleeping app's host, where
/// `/logo.svg` would be one more request to the app (answered with this page, not an image).
static LOGO_DATA_URL: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| data_url(include_str!("../../assets/logo/sds-logo.svg")));

fn data_url(svg: &str) -> String {
    let mut out = String::from("data:image/svg+xml,");
    for b in svg.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~!$'()*+,;=:@/?".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub struct WaitPage<'a> {
    pub group: &'a str,
    pub title: &'a str,
    pub message: &'a str,
    pub status: StatusCode,
    /// Seconds after which the browser reloads (`Refresh` header); `None` for no reload.
    pub refresh_secs: Option<u32>,
    pub state: &'a str,
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

impl WaitPage<'_> {
    pub fn html(&self) -> String {
        TEMPLATE
            .replace("{{group}}", &escape(self.group))
            .replace("{{title}}", &escape(self.title))
            .replace("{{message}}", &escape(self.message))
            .replace("{{state}}", &escape(self.state))
            .replace("{{refresh}}", &self.refresh_secs.map(|s| s.to_string()).unwrap_or_default())
            .replace("{{logo}}", &LOGO_DATA_URL)
    }

    pub fn into_response(self, wants_html: bool) -> Response {
        let mut resp = if wants_html {
            (self.status, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], self.html()).into_response()
        } else {
            let body = serde_json::json!({
                "group": self.group,
                "state": self.state,
                "message": self.message,
            });
            (self.status, axum::Json(body)).into_response()
        };
        let headers = resp.headers_mut();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Ok(v) = HeaderValue::from_str(self.state) {
            headers.insert("x-sds-state", v);
        }
        if let Some(secs) = self.refresh_secs {
            if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                headers.insert(header::RETRY_AFTER, v.clone());
                if wants_html {
                    headers.insert("refresh", v);
                }
            }
        }
        resp
    }
}

/// Browsers send `Accept: text/html`; API clients, curl and health checkers do not.
pub fn wants_html(headers: &axum::http::HeaderMap) -> bool {
    headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|v| v.contains("text/html"))
}

/// Human-readable description of a non-ready [`WakeOutcome`], shared by the forward-auth
/// endpoint, the native HTTP proxy and the API so all three tell the same story.
pub struct OutcomeText {
    pub title: &'static str,
    pub message: String,
    /// Value of the `X-SDS-State` header.
    pub state: &'static str,
    /// Suggested `Retry-After` / page refresh in seconds.
    pub retry_secs: u32,
}

impl OutcomeText {
    pub fn of(outcome: &WakeOutcome) -> Self {
        let (title, message, state, retry_secs) = match outcome {
            WakeOutcome::Ready => ("Ready", "The service is up.".to_owned(), "running", 0),
            WakeOutcome::Starting => (
                "Your service will be up in a moment",
                "It was asleep to save resources and is starting now. This page reloads automatically.".to_owned(),
                "starting",
                3,
            ),
            WakeOutcome::Timeout => (
                "Almost there",
                "Starting takes a little longer than usual. This page keeps retrying.".to_owned(),
                "starting",
                5,
            ),
            WakeOutcome::Failed(_) => (
                "Start failed",
                "The service could not be started. sds retries shortly.".to_owned(),
                "failed",
                10,
            ),
            WakeOutcome::Overloaded => (
                "Busy",
                "Too many requests are waiting for this service to start. Please retry in a moment.".to_owned(),
                "overloaded",
                5,
            ),
            WakeOutcome::Gone => ("Service removed", "The service disappeared while waiting.".to_owned(), "gone", 10),
            WakeOutcome::Held => (
                "On hold",
                "The service has been stopped by an operator and is not started until it is released.".to_owned(),
                "held",
                30,
            ),
        };
        Self { title, message, state, retry_secs }
    }

    /// The waiting page for this outcome with the given status code.
    pub fn page<'a>(&'a self, group: &'a str, status: StatusCode) -> WaitPage<'a> {
        WaitPage {
            group,
            title: self.title,
            message: &self.message,
            status,
            refresh_secs: Some(self.retry_secs),
            state: self.state,
        }
    }
}

/// Shared with the native HTTP proxy: (status, html, json, retry-after seconds) for a
/// non-ready outcome.
pub fn page_for_outcome(group: &str, outcome: &WakeOutcome) -> (StatusCode, String, String, u32) {
    let text = OutcomeText::of(outcome);
    let status = if matches!(outcome, WakeOutcome::Ready) { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    let json = serde_json::json!({ "group": group, "state": text.state, "message": text.message }).to_string();
    (status, text.page(group, status).html(), json, text.retry_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_user_controlled_text() {
        let page = WaitPage {
            group: "<b>x</b>",
            title: "t",
            message: "a & b",
            status: StatusCode::SERVICE_UNAVAILABLE,
            refresh_secs: Some(3),
            state: "starting",
        };
        let html = page.html();
        assert!(html.contains("&lt;b&gt;x&lt;/b&gt;"));
        assert!(html.contains("a &amp; b"));
        assert!(!html.contains("{{"));
    }

    #[test]
    fn page_needs_no_second_request_for_the_logo() {
        let html = OutcomeText::of(&WakeOutcome::Starting).page("shop", StatusCode::SERVICE_UNAVAILABLE).html();
        assert!(!html.contains("/logo.svg"));
        assert!(html.contains("src=\"data:image/svg+xml,"));
        assert!(!LOGO_DATA_URL.contains('"') && !LOGO_DATA_URL.contains('#') && !LOGO_DATA_URL.contains('<'));
    }

    #[test]
    fn public_failure_pages_do_not_expose_docker_errors() {
        let secret = "docker start `db-internal`: connection details";
        let html = OutcomeText::of(&WakeOutcome::Failed(secret.into()))
            .page("shop", StatusCode::SERVICE_UNAVAILABLE)
            .html();
        assert!(!html.contains("db-internal"));
        assert!(!html.contains("connection details"));
        assert!(html.contains("could not be started"));
    }
}
