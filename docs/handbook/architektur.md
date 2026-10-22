# Architektur (für Entwickler)

`sds` ist ein einzelnes Rust-Binary (Edition 2024, tokio) mit Bibliotheks- und
CLI-Teil. Die Module folgen den fachlichen Grenzen:

```text
src/
├── config/       sds.toml, Label-Parsing, effektive Gruppen-Einstellungen
├── docker/       Backend-Trait, bollard-Implementierung, Discovery (Container -> Gruppen)
├── controller/   Zustandsmaschine pro Gruppe, Aktivitätszähler, Registry, Supervisor
├── admin/        axum: API, Forward-Auth, Warteseite, SSE, eingebettete Web-Übersicht
├── proxy/        nativer TCP-/HTTP-Proxy (hyper)
├── integrate/    Snippet-Generatoren für nginx, Caddy, Traefik
├── service/      systemd-/launchd-Units
├── tui/          ratatui-Dashboard, Log-Ringpuffer
└── cli/          clap-Befehle, API-Client
```

## Datenfluss

```text
 Docker events ──┐                             ┌── /_sds/check (axum)
 Docker list ────┴─▶ Supervisor ─▶ Registry ───┤── /api/v1/*   (axum)
                     (discover)   (1 Actor     ├── nativer Proxy (hyper)
                                   pro Gruppe) └── TUI / Web-Übersicht
                                        │
                                        ▼
                                  Backend-Trait ─▶ bollard ─▶ Docker Engine API
```

- **Supervisor** listet Container periodisch und bei Create/Destroy/Rename-Events, baut
  mit `discover()` deterministische `GroupSpec`s und ruft `Registry::apply`.
- **Registry** hält pro Gruppe einen Actor (tokio-Task), dessen `GroupHandle` und optional
  einen Proxy-Listener. Neue Gruppen werden gestartet, geänderte per `Command::Update`
  informiert, verschwundene beendet.
- **Actor** besitzt die Zustandsmaschine (`stopped → starting → running → stopping`,
  `failed`). Lange Operationen (`docker start` + Health-Probing, `docker stop`) laufen in
  eigenen Tasks, damit der Actor auf Kommandos und Events reagieren kann. Der Zustand
  wird als `GroupSnapshot` über einen `watch`-Channel veröffentlicht.
- **GroupHandle** ist die Schnittstelle für Request-Pfade: `touch()` (zwei Atomics),
  `ensure_ready()` (wartet auf dem `watch`-Channel), `request_wake()` (`try_send`).

## Garantien im Request-Pfad

- Kein Docker-Aufruf im Request-Pfad. Handler lesen den `watch`-Snapshot und schreiben
  Atomics in `Activity`.
- Ein Start pro Gruppe, egal wie viele Anfragen warten: `Command::Wake` ist idempotent,
  der Actor startet nur aus `stopped`/`failed`.
- Gehaltene Anfragen sind durch `max_waiters` begrenzt; darüber hinaus sofort 503.
- Kein Pufferung ganzer Bodies im nativen Proxy; `copy_bidirectional` bzw. hyper-Streams
  mit Backpressure über TCP.
- Stopp nur bei `inflight == 0` und abgelaufenem `idle_timeout`; ein Wake während
  `stopping` wird vorgemerkt und direkt danach ausgeführt.
- Nach einem fehlgeschlagenen Start werden Anfragen 10 s lang sofort beantwortet
  (`FAILED_RETRY_INTERVAL_MS`), danach löst die nächste einen neuen Versuch aus.

## Tests

- Unit-Tests pro Modul (`cargo test`), darunter die Zustandsmaschine gegen ein
  In-Memory-`MockBackend` (`controller/actor.rs`): Wecken, Idle-Stop, offene
  Verbindungen, Fehlstart mit Rate-Limit, externe Start/Stop-Events, Wake während Stop.
- Snippet-Generatoren und Unit-Dateien werden auf die entscheidenden Zeilen geprüft.
- CLI-Tests (`tests/cli.rs`) über `assert_cmd`.
- Manuelle End-to-End-Prüfung gegen echtes Docker: `examples/native-proxy` starten,
  `sds run`, `sds sleep`, `curl` gegen Proxy und Check-Endpunkt.

## Release

Conventional Commits, commitizen und GitLab-CI nach ADR 0005 des Engineering-Handbooks:
`fix` → Patch, `feat` → Minor, `!` → Major; andere Typen lösen kein Release aus. Die
Pipeline auf `main` ermittelt die Version, baut Image und Binary und legt Tag plus
GitLab-Release mit dem Job-Token an.

## Erweiterungspunkte

- Weiterer Proxy-Typ: `integrate/mod.rs` um eine Render-Funktion ergänzen.
- Anderes Container-Backend (Podman spricht dieselbe API; andere): `docker::Backend`
  implementieren.
- Config-Reload: Supervisor und Admin-Server erhalten einen `watch::Receiver<Arc<Config>>`;
  die Signalbehandlung in `cli/run.rs` ist bereits vorbereitet.
- Metriken: `Registry::snapshots()` liefert alles, was ein Prometheus-Endpunkt braucht.
