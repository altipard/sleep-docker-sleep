# Betrieb

## Befehle im Überblick

| Befehl | Zweck |
|---|---|
| `sds run [--headless] [--bind ADDR]` | Daemon starten. Im Terminal mit Dashboard, ohne TTY oder mit `--headless` nur Logs. |
| `sds status [--json] [--events]` | Alle Gruppen mit Zustand, Containern, letzter Anfrage, verbleibender Zeit bis zum Schlafen. |
| `sds wake <gruppe> [--wait]` | Gruppe starten; mit `--wait` blockiert der Befehl bis zur Bereitschaft (Exit-Code ≠ 0 bei Fehler). |
| `sds sleep <gruppe>` | Gruppe sofort stoppen. |
| `sds check [--json]` | Konfiguration, Docker-Zugriff und Gruppen prüfen, ohne Daemon. Exit-Code 1 bei Warnungen. |
| `sds init [--path] [--force] [--stdout]` | Kommentierte `sds.toml` schreiben. |
| `sds integrate <nginx\|caddy\|traefik> <gruppe> …` | Proxy-Snippet ausgeben. |
| `sds service show\|install\|uninstall` | systemd-/launchd-Unit anzeigen, installieren, Entfernbefehle zeigen. |
| `sds config [--path]` | Effektive Konfiguration bzw. den verwendeten Pfad zeigen. |

`status`, `wake` und `sleep` sprechen mit dem laufenden Daemon. Sie lesen `admin.bind` und
`admin.token` aus `sds.toml`; abweichend per `--url` / `--token` oder den
Umgebungsvariablen `SDS_URL` / `SDS_TOKEN`.

## Dashboard im Terminal

`sds run` in einer interaktiven Sitzung zeigt Gruppen, Zustand, Container, Anfragen und
ein Log-Fenster. Tasten: `↑`/`↓` auswählen, `w` wecken, `s` schlafen legen, `q` beenden.
Beim Beenden bleiben die Container, wie sie sind.

## Web-Übersicht

Unter `http://<admin.bind>/` (Standard `http://127.0.0.1:9900/`) liegt eine Seite mit
derselben Information, live über Server-Sent Events: Gruppen, Countdown bis zum
Einschlafen, Warnungen, Ereignisprotokoll, Buttons für Wecken und Schlafen. Ein Klick auf
eine Zeile zeigt die effektiven Einstellungen und die Forward-Auth-URL.

Ist ein Token gesetzt, fragt die Seite ihn einmal ab und speichert ihn im Browser. Von
außerhalb des Hosts erreichbar machen nur hinter einem Reverse-Proxy mit TLS.

## Zustände einer Gruppe

| Zustand | Bedeutung |
|---|---|
| `stopped` | Alle Mitglieder gestoppt. Nächste Anfrage weckt. |
| `starting` | `docker start` läuft oder Health-Check noch nicht grün. Anfragen werden gehalten bzw. sehen die Warteseite. |
| `running` | Alle Mitglieder laufen, Verkehr fließt. `sleeps in` zeigt die Restzeit. |
| `stopping` | `docker stop` läuft. Eine Anfrage in dieser Phase führt direkt danach zu einem Neustart. |
| `failed` | Letzter Start fehlgeschlagen (Grund in `notes`/`last_error`). Für 10 s werden Anfragen sofort mit 503 beantwortet, danach löst die nächste Anfrage einen neuen Versuch aus. |

Externe Eingriffe werden erkannt: `docker compose stop` führt zu `stopped`,
`docker compose up -d` zu `running` mit frischem Idle-Timer. Das Ereignisprotokoll
vermerkt sie als `external`.

## Logs

- systemd: `journalctl -u sds -f`
- launchd: `/var/log/sds.log`
- Container: `docker logs -f sds`

Mit `log.json = true` kommen JSON-Zeilen. Jedes relevante Ereignis (discovered, waking,
running, sleeping, stopped, start_failed, warning, external) erscheint mit Gruppenname.
Detailgrad über `log.filter` oder `SDS_LOG=debug`.

## Monitoring

- `GET /healthz` antwortet `ok`, solange der Daemon läuft (für Uptime-Checks, Load-Balancer).
- `sds status --json` oder `GET /api/v1/status` liefern alle Gruppen maschinenlesbar,
  z. B. für einen Prometheus-Exporter oder ein Dashboard über mehrere Hosts.
- `sds check` im Deployment-Pipeline-Schritt bricht bei Warnungen ab (Exit-Code 1).

## Was beim Stoppen des Daemons passiert

`sds` stoppt beim Beenden keine Container und startet keine. Laufende Gruppen laufen
weiter, bis `sds` wieder läuft und ihr `idle_timeout` ab dem Neustart erneut abläuft;
schlafende Gruppen werden ohne `sds` nicht geweckt (nginx leitet per Fail-open an den
gestoppten Port und liefert 502, Caddy/Traefik liefern 502/500). Deshalb den Dienst mit
automatischem Neustart betreiben; die mitgelieferten Units tun das.

## Sicherheit

- Der Docker-Socket entspricht Root. `sds` nur unter einem vertrauenswürdigen Benutzer
  betreiben.
- `admin.bind` auf `127.0.0.1` lassen, wo es geht. Sonst `admin.token` setzen und den
  Port nicht öffentlich veröffentlichen.
- Die Warteseite gibt nur den Gruppennamen und den Zustand preis, keine Container-IDs.
- Der Check-Endpunkt kann Gruppen wecken. Mit Token ist er nur für den Proxy nutzbar.

## Backup

`sds` hält keinen persistenten Zustand. Zu sichern ist nur `sds.toml`.
