# Schnellstart

Fünf Schritte vom leeren Host bis zur ersten schlafenden Gruppe. Details stehen in den
Folgekapiteln.

## 1. Binary installieren und prüfen

```bash
sudo install -m 755 sds /usr/local/bin/sds
sds --version
sds check            # prüft Docker-Zugriff; "no groups" ist jetzt noch in Ordnung
```

## 2. Konfiguration anlegen

```bash
sudo mkdir -p /etc/sds
sds init --path /etc/sds/sds.toml
```

Die Datei enthält Kommentare zu jedem Feld. Für einen ersten Test kann sie unverändert
bleiben: Admin-API auf `127.0.0.1:9900`, `idle_timeout` 15 Minuten.

## 3. Container labeln

Im Compose-File des Projekts:

```yaml
services:
  app:
    image: …
    ports:
      - "127.0.0.1:18080:80"     # nur noch lokal; der Reverse-Proxy spricht 18080 an
    labels:
      sds.enable: "true"
      sds.idle_timeout: "15m"
      sds.health: "http://127.0.0.1:18080/healthz"
  db:
    image: …
    labels:
      sds.enable: "true"         # gleiches Compose-Projekt = gleiche Gruppe
```

```bash
docker compose up -d
sds check                        # zeigt die Gruppe und ihre Einstellungen
```

Der Gruppenname ist der Compose-Projektname (Ordnername), sofern kein `sds.group` gesetzt
ist.

## 4. Daemon starten

Zum Ausprobieren im Vordergrund mit Dashboard:

```bash
sds run --config /etc/sds/sds.toml
```

Dauerhaft als Dienst:

```bash
sudo sds service install --config /etc/sds/sds.toml
sudo systemctl enable --now sds          # macOS: siehe Ausgabe von service install
sds status
```

## 5. Reverse-Proxy anbinden

```bash
sds integrate nginx <gruppe> --host app.example.com --upstream http://127.0.0.1:18080
```

Die Ausgabe ist ein fertiger `server`-Block (für Caddy ein Caddyfile-Block, für Traefik
eine Middleware-Definition). Einbauen, Proxy neu laden, fertig.

Test: `sds sleep <gruppe>`, dann die Seite im Browser öffnen. Es erscheint kurz die
Warteseite oder die Seite lädt nach ein bis zwei Sekunden, danach ist die Gruppe wieder
`running` in `sds status` und unter `http://127.0.0.1:9900/`.
