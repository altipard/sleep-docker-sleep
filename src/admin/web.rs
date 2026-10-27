use axum::http::header;
use axum::response::{Html, IntoResponse, Response};

const INDEX: &str = include_str!("assets/index.html");
const DOCS: &str = include_str!("assets/docs.html");
const OPENAPI: &str = include_str!("assets/openapi.json");
const LOGO: &str = include_str!("../../assets/logo/sds-logo.svg");

pub async fn index() -> Response {
    let mut resp = Html(INDEX).into_response();
    resp.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    resp
}

pub async fn logo() -> Response {
    ([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "public, max-age=86400")], LOGO).into_response()
}

pub async fn docs() -> Response {
    Html(DOCS).into_response()
}

/// The hand-written spec with `info.version` set to the running binary's version.
pub async fn openapi() -> Response {
    let mut spec: serde_json::Value = match serde_json::from_str(OPENAPI) {
        Ok(v) => v,
        Err(_) => return ([(header::CONTENT_TYPE, "application/json")], OPENAPI).into_response(),
    };
    spec["info"]["version"] = serde_json::Value::from(crate::VERSION);
    axum::Json(spec).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every route registered in `admin/mod.rs` must be documented, and vice versa.
    #[test]
    fn spec_documents_exactly_the_routes() {
        let spec: serde_json::Value = serde_json::from_str(OPENAPI).expect("openapi.json is valid JSON");
        let documented: std::collections::BTreeSet<&str> =
            spec["paths"].as_object().expect("paths").keys().map(String::as_str).collect();
        let pages = ["/", "/logo.svg", "/docs", "/openapi.json"];
        let routed: std::collections::BTreeSet<&str> = include_str!("mod.rs")
            .split(".route(\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .filter(|path| !pages.contains(path))
            .collect();
        assert_eq!(documented, routed);
    }
}
