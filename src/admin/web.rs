use axum::http::header;
use axum::response::{Html, IntoResponse, Response};

const INDEX: &str = include_str!("assets/index.html");

pub async fn index() -> Response {
    let mut resp = Html(INDEX).into_response();
    resp.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    resp
}
