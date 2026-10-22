# sleep-docker-sleep (`sds`)

Stoppt Docker-Container, die eine Weile keine Anfragen bekommen, und weckt sie beim
nächsten Request wieder auf. Ein Rust-Binary, das sich an den vorhandenen Reverse-Proxy
(nginx, Caddy, Traefik) anhängt und selbst keinen Datenverkehr tragen muss.

```text
Browser ─▶ Reverse-Proxy ─(forward-auth)─▶ sds ─▶ docker start/stop
                 └──────────────────────────────▶ Container-Gruppe
```

- Container beschreiben sich per Label (`sds.enable=true`, `sds.idle_timeout=15m`).
- Eine Gruppe (standardmäßig ein Compose-Projekt) schläft und wacht als Einheit.
- Beim Aufwecken wird die Anfrage gehalten oder eine Warteseite gezeigt.
- Nativer TCP/HTTP-Proxy für Stacks ohne Reverse-Proxy.
- CLI, Terminal-Dashboard, Web-Übersicht und JSON-API aus einem Prozess.

## Schnellstart

```bash
sds init --path /etc/sds/sds.toml        # Konfiguration mit Kommentaren
sds check                                # Docker-Zugriff und Gruppen prüfen
sds run                                  # Daemon mit Dashboard
sds integrate nginx <gruppe> --host app.example.com --upstream http://127.0.0.1:18080
```

Vollständige Anleitung: [Handbuch](docs/handbook/README.md) (Installation, Konfiguration,
Proxy-Anbindung, Betrieb, Fehlersuche, API).

## Entwicklung

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo run -- run --config sds.toml
```

Beispiele für nginx, Caddy, Traefik und den nativen Proxy liegen unter `examples/`.
Commits folgen Conventional Commits; Releases entstehen automatisch aus `main`.

## Lizenz

MIT, siehe `LICENSE`.
