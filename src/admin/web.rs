use axum::http::header;
use axum::response::{Html, IntoResponse, Response};

const INDEX: &str = include_str!("assets/index.html");
const LOGO: &str = include_str!("../../assets/logo/sds-logo.svg");

pub async fn index() -> Response {
    let mut resp = Html(INDEX).into_response();
    resp.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    resp
}

pub async fn logo() -> Response {
    ([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "public, max-age=86400")], LOGO).into_response()
}
