use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::SharedState;

pub const TOKEN_HEADER: &str = "x-sds-token";

/// When `admin.token` is set, `/api/*` and `/_sds/check/*` require it as
/// `Authorization: Bearer <token>` or `X-SDS-Token: <token>`.
pub async fn require_token(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    let Some(expected) = state.config.admin.token.as_deref() else {
        return next.run(req).await;
    };
    if token_matches(req.headers(), expected) {
        return next.run(req).await;
    }
    (StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer realm=\"sds\"")], "missing or invalid token\n")
        .into_response()
}

fn token_matches(headers: &HeaderMap, expected: &str) -> bool {
    let presented = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()).or_else(|| {
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
    });
    // An empty expected token is rejected by `Config::validate`; never let it match anyway.
    !expected.is_empty() && presented.is_some_and(|p| constant_time_eq(p.as_bytes(), expected.as_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_bearer_and_custom_header() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer s3cret".parse().unwrap());
        assert!(token_matches(&h, "s3cret"));
        let mut h = HeaderMap::new();
        h.insert(TOKEN_HEADER, "s3cret".parse().unwrap());
        assert!(token_matches(&h, "s3cret"));
        assert!(!token_matches(&h, "other"));
        assert!(!token_matches(&HeaderMap::new(), "s3cret"));
        let mut h = HeaderMap::new();
        h.insert(TOKEN_HEADER, "".parse().unwrap());
        assert!(!token_matches(&h, ""));
    }
}
