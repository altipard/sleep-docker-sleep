# Installation

## Voraussetzungen

- Docker Engine 20.10 oder neuer (API 1.41+), Docker Compose v2 optional.
- Linux (systemd) oder macOS (launchd). Windows wird nicht unterstützt.
- Zugriff auf den Docker-Socket (`/var/run/docker.sock`) für den Benutzer, unter dem `sds`
  läuft. Hinweis: Wer den Socket lesen und schreiben darf, hat faktisch Root-Rechte auf
  dem Host. `sds` daher als `root` oder als dedizierten Benutzer in der Gruppe `docker`
  betreiben, nicht als Anwendungsnutzer.

## Variante A: Binary auf dem Host (empfohlen)

Das Release enthält ein statisch gelinktes Linux-Binary (`sds-linux-x86_64`). Auf macOS
oder anderen Architekturen aus dem Quellcode bauen:

```bash
cargo build --release          # Rust 1.85+
sudo install -m 755 target/release/sds /usr/local/bin/sds
```

Konfiguration nach `/etc/sds/sds.toml` (siehe [Konfiguration](konfiguration.md)), dann:

```bash
sudo sds service install --config /etc/sds/sds.toml
```

`service install` schreibt die passende Unit (`/etc/systemd/system/sds.service` bzw.
`/Library/LaunchDaemons/com.zilicon.sds.plist`) und gibt die Befehle zum Aktivieren aus.
`sds service show` zeigt die Unit nur an, ohne etwas zu schreiben. Mit `--user <name>`
läuft der Dienst unter einem anderen Benutzer.

systemd:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now sds
journalctl -u sds -f
```

launchd:

```bash
sudo launchctl bootstrap system /Library/LaunchDaemons/com.zilicon.sds.plist
tail -f /var/log/sds.log
```

## Variante B: als Container

Sinnvoll, wenn der Reverse-Proxy selbst als Container läuft (Traefik, projekteigener
Caddy) und `sds` im selben Docker-Netz erreichbar sein soll. Vorlage:
`examples/sds-as-container.compose.yaml`.

```yaml
services:
  sds:
    image: registry.gitlab.com/zilicon-it-services/sleep-docker-sleep:<version>
    restart: unless-stopped
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - ./sds.toml:/etc/sds/sds.toml:ro
    networks: [proxy]
    labels:
      sds.keep_awake: "true"
```

In `sds.toml` muss dann `admin.bind = "0.0.0.0:9900"` stehen, und der Reverse-Proxy
verwendet `http://sds:9900/_sds/check/<gruppe>`. Den Port **nicht** nach außen
veröffentlichen, oder einen `admin.token` setzen.

Health-Checks mit `http://…` oder `tcp://…` werden aus dem `sds`-Container heraus
ausgeführt. Adressen also so angeben, wie sie von dort erreichbar sind (`http://app:3000/`
statt `127.0.0.1:18080`).

## Mehrere Hosts

Ein `sds` pro Host. Es gibt bewusst keine zentrale Instanz; jede verwaltet nur den
lokalen Docker-Daemon. Eine Übersicht über mehrere Hosts lässt sich über die
[HTTP-API](api.md) (`/api/v1/status`) bauen.

## Aktualisieren

Binary austauschen und Dienst neu starten:

```bash
sudo install -m 755 sds /usr/local/bin/sds && sudo systemctl restart sds
```

Beim Neustart bleiben laufende Container laufen und schlafende schlafen; `sds` liest den
Ist-Zustand beim Start aus Docker. Anfragen während des Neustarts laufen bei nginx per
Fail-open an die Anwendung durch; bei Caddy und Traefik gibt es für die Dauer des
Neustarts (unter einer Sekunde) Fehler 502/500, siehe [Grenzen](grenzen.md).
