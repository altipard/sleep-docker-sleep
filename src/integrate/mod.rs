//! Reverse-proxy snippets for the forward-auth integration. The proxy-specific knowledge
//! (which status codes survive, how to fail open) lives here, not in the handbook.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    Nginx,
    Caddy,
    Traefik,
}

impl FromStr for ProxyKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "nginx" => Ok(Self::Nginx),
            "caddy" => Ok(Self::Caddy),
            "traefik" => Ok(Self::Traefik),
            other => Err(format!("unknown proxy `{other}` (expected nginx, caddy or traefik)")),
        }
    }
}

impl fmt::Display for ProxyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nginx => f.write_str("nginx"),
            Self::Caddy => f.write_str("caddy"),
            Self::Traefik => f.write_str("traefik"),
        }
    }
}

/// Everything a snippet needs to know.
#[derive(Debug, Clone)]
pub struct SnippetInput {
    pub group: String,
    /// How the proxy reaches `sds`, e.g. `http://127.0.0.1:9900` or `http://host.docker.internal:9900`.
    pub sds_url: String,
    /// The application upstream as the proxy sees it, e.g. `http://127.0.0.1:18080` or `http://app:3000`.
    pub upstream: String,
    /// Public hostname (used for the server block / router rule).
    pub host: String,
    pub token: Option<String>,
    /// Must cover `start_timeout` when `wake = hold`.
    pub hold_timeout_secs: u64,
}

pub fn render(kind: ProxyKind, input: &SnippetInput) -> String {
    match kind {
        ProxyKind::Nginx => nginx(input),
        ProxyKind::Caddy => caddy(input),
        ProxyKind::Traefik => traefik(input),
    }
}

fn nginx(i: &SnippetInput) -> String {
    let token_line =
        i.token.as_ref().map(|t| format!("        proxy_set_header X-SDS-Token \"{t}\";\n")).unwrap_or_default();
    let read_timeout = i.hold_timeout_secs + 5;
    format!(
        r#"# sleep-docker-sleep: forward-auth integration for group "{group}"
# nginx `auth_request` only accepts 2xx (allow) and 401/403 (deny); everything else becomes
# a 500. sds therefore answers "not ready" with 403 here (?deny=403), and `error_page`
# turns that into the waiting page. A failure to reach sds at all (500/502) falls through
# to the application unchanged (fail-open), so an sds outage never blocks traffic.

upstream sds_{group_ident}_app {{
    server {upstream_host};
    keepalive 16;
}}

server {{
    listen 80;
    server_name {host};

    location / {{
        auth_request /_sds_check;
        error_page 403 = @sds_wait;      # sleeping: show the waiting page
        error_page 500 502 = @sds_bypass; # sds unreachable: fail open

        proxy_pass http://sds_{group_ident}_app;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
    }}

    location = /_sds_check {{
        internal;
        proxy_pass {sds_url}/_sds/check/{group}?deny=403;
        proxy_pass_request_body off;
        proxy_set_header Content-Length "";
        proxy_set_header X-Original-URI $request_uri;
        proxy_set_header X-Forwarded-Host $host;
{token_line}        # must exceed the group's hold_timeout when wake = hold
        proxy_read_timeout {read_timeout}s;
        proxy_connect_timeout 2s;
    }}

    location @sds_wait {{
        proxy_pass {sds_url}/_sds/wait/{group};
        proxy_set_header Accept "text/html";
        proxy_intercept_errors off;
    }}

    location @sds_bypass {{
        proxy_pass http://sds_{group_ident}_app;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
    }}
}}

# Put this once in the http {{}} context if it is not there yet:
# map $http_upgrade $connection_upgrade {{ default upgrade; '' close; }}
"#,
        group = i.group,
        group_ident = i.group.replace(['-', '.'], "_"),
        upstream_host = i.upstream.trim_start_matches("http://").trim_start_matches("https://"),
        host = i.host,
        sds_url = i.sds_url.trim_end_matches('/'),
        token_line = token_line,
        read_timeout = read_timeout,
    )
}

fn caddy(i: &SnippetInput) -> String {
    let token_line = i.token.as_ref().map(|t| format!("        header_up X-SDS-Token \"{t}\"\n")).unwrap_or_default();
    format!(
        r#"# sleep-docker-sleep: forward-auth integration for group "{group}"
# Caddy's forward_auth lets a non-2xx answer from sds through to the client unchanged, so
# the waiting page (503 + Refresh) is served directly. Note: Caddy has no clean fail-open
# for forward_auth; if sds is unreachable the client sees a 502. Run sds as a supervised
# service (systemd/launchd); a restart takes well under a second.

{host} {{
    forward_auth {sds_host} {{
        uri /_sds/check/{group}
        copy_headers X-SDS-State
{token_line}        # must exceed the group's hold_timeout when wake = hold
        transport http {{
            response_header_timeout {hold}s
            dial_timeout 2s
        }}
    }}

    reverse_proxy {upstream}
}}
"#,
        group = i.group,
        host = i.host,
        sds_host = i.sds_url.trim_start_matches("http://").trim_end_matches('/'),
        token_line = token_line,
        hold = i.hold_timeout_secs + 5,
        upstream = i.upstream,
    )
}

fn traefik(i: &SnippetInput) -> String {
    let token_block = i
        .token
        .as_ref()
        .map(|t| format!("          authRequestHeaders: []\n          # the token travels in the address, Traefik does not add headers here\n          # alternatively run sds without a token on a private network\n          address: \"{}/_sds/check/{}?token={t}\"\n", i.sds_url.trim_end_matches('/'), i.group))
        .unwrap_or_default();
    let address_line = if i.token.is_some() {
        token_block
    } else {
        format!("          address: \"{}/_sds/check/{}\"\n", i.sds_url.trim_end_matches('/'), i.group)
    };
    format!(
        r#"# sleep-docker-sleep: forward-auth integration for group "{group}"
# Traefik's forwardAuth returns a non-2xx answer from sds to the client unchanged, so the
# waiting page (503 + Refresh) is served directly. Note: Traefik has no fail-open for
# forwardAuth; if sds is unreachable the client sees a 500. Run sds as a supervised
# service (systemd/launchd) and keep the middleware's timeout generous.

# --- dynamic configuration (file provider) ------------------------------------------
http:
  middlewares:
    sds-{group}:
      forwardAuth:
{address_line}          # only these headers are forwarded to sds; keep it minimal
          authResponseHeaders:
            - X-SDS-State
          # must exceed the group's hold_timeout when wake = hold (Traefik >= 3.3)
          forwardAuthTimeout: {hold}s

  routers:
    {group}:
      rule: "Host(`{host}`)"
      middlewares:
        - sds-{group}
      service: {group}

  services:
    {group}:
      loadBalancer:
        servers:
          - url: "{upstream}"

# --- or as container labels on the application -------------------------------------
#   labels:
#     traefik.enable: "true"
#     traefik.http.routers.{group}.rule: "Host(`{host}`)"
#     traefik.http.routers.{group}.middlewares: "sds-{group}"
#     traefik.http.middlewares.sds-{group}.forwardauth.address: "{sds_url}/_sds/check/{group}"
#     traefik.http.middlewares.sds-{group}.forwardauth.authResponseHeaders: "X-SDS-State"
#     sds.enable: "true"
#     sds.group: "{group}"
"#,
        group = i.group,
        address_line = address_line,
        hold = i.hold_timeout_secs + 5,
        host = i.host,
        upstream = i.upstream,
        sds_url = i.sds_url.trim_end_matches('/'),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> SnippetInput {
        SnippetInput {
            group: "shop".into(),
            sds_url: "http://127.0.0.1:9900".into(),
            upstream: "http://127.0.0.1:18080".into(),
            host: "shop.example.com".into(),
            token: Some("t0k".into()),
            hold_timeout_secs: 60,
        }
    }

    #[test]
    fn nginx_uses_403_and_fails_open() {
        let s = render(ProxyKind::Nginx, &input());
        assert!(s.contains("/_sds/check/shop?deny=403"));
        assert!(s.contains("error_page 403 = @sds_wait"));
        assert!(s.contains("error_page 500 502 = @sds_bypass"));
        assert!(s.contains("X-SDS-Token \"t0k\""));
        assert!(s.contains("proxy_read_timeout 65s"));
    }

    #[test]
    fn caddy_and_traefik_mention_group_and_upstream() {
        for kind in [ProxyKind::Caddy, ProxyKind::Traefik] {
            let s = render(kind, &input());
            assert!(s.contains("/_sds/check/shop"), "{kind}");
            assert!(s.contains("http://127.0.0.1:18080"), "{kind}");
            assert!(s.contains("shop.example.com"), "{kind}");
        }
    }

    #[test]
    fn parses_kind() {
        assert_eq!("Nginx".parse::<ProxyKind>(), Ok(ProxyKind::Nginx));
        assert!("haproxy".parse::<ProxyKind>().is_err());
    }
}
