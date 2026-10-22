# Konfiguration (`sds.toml`)

`sds` kommt ohne Datei aus (eingebaute Standardwerte, Admin-API auf `127.0.0.1:9900`).
Für den Betrieb empfiehlt sich trotzdem eine Datei, mindestens für Token und Logging.

## Suchreihenfolge

1. `--config <pfad>` (Fehler, wenn die Datei fehlt)
2. Umgebungsvariable `SDS_CONFIG`
3. `./sds.toml` im aktuellen Verzeichnis
4. `~/.config/sds/sds.toml`
5. `/etc/sds/sds.toml`

`sds config --path` zeigt, welche Datei verwendet wird; `sds config` gibt die
effektive Konfiguration aus. `sds init` schreibt eine kommentierte Vorlage.

Unbekannte Schlüssel sind Fehler, damit Tippfehler nicht stillschweigend ignoriert werden.
Alle Zeiten verwenden die `humantime`-Schreibweise: `30s`, `15m`, `2h`, `1h 30m`.

## Vorrang der Einstellungen

Für jede Gruppe gilt von schwach nach stark:

1. eingebaute Standardwerte
2. `[defaults]` in `sds.toml`
3. `[groups.<name>]` in `sds.toml`
4. Container-Labels (`sds.*`)

Setzen mehrere Container derselben Gruppe unterschiedliche Werte, gewinnt der
alphabetisch erste Container; `sds check` und die Web-Übersicht zeigen eine Warnung.

## Referenz

```toml
[admin]
bind = "127.0.0.1:9900"      # Admin-API, Forward-Auth-Endpunkt und Web-Übersicht
# token = "…"                 # Pflicht, sobald bind nicht loopback ist
# public_url = "https://…"    # nur, wenn die Warteseite über eine andere Origin geladen wird

[docker]
# host = "unix:///var/run/docker.sock"   # oder tcp://…; Standard: DOCKER_HOST bzw. Socket

[discovery]
interval = "30s"             # Vollabgleich aller Container; Docker-Events lösen ihn zusätzlich sofort aus

[log]
filter = "info"              # tracing-Filter, z. B. "info,sds=debug"
json = false                 # true: JSON-Zeilen für journald/Log-Shipper

[defaults]                   # gilt für alle Gruppen, von Labels überschreibbar
idle_timeout = "15m"         # Stopp nach so viel Zeit ohne Anfrage
start_timeout = "90s"        # Aufwecken gilt als gescheitert, wenn bis dahin nicht gesund
stop_timeout = "20s"         # Gnadenfrist für docker stop vor SIGKILL
start_grace = "5s"           # Wartezeit bei health = grace oder ohne HEALTHCHECK
health = "docker"            # docker | grace | "http://…" | "tcp://host:port"
wake = "hold"                # hold | page
hold_timeout = "60s"         # maximale Haltezeit einer Anfrage beim Aufwecken
max_waiters = 200            # mehr gleichzeitig gehaltene Anfragen -> sofort 503

[groups.legacy]              # optional: Gruppen ohne Labels
containers = ["legacy-web-1", "legacy-db-1"]
idle_timeout = "30m"         # alle Felder aus [defaults] sind hier erlaubt

[groups.legacy.proxy]        # optional: nativer Proxy, siehe eigenes Kapitel
listen = "0.0.0.0:8080"
upstream = "127.0.0.1:18080"
mode = "http"                # http | tcp
```

### `[admin]`

| Feld | Standard | Bedeutung |
|---|---|---|
| `bind` | `127.0.0.1:9900` | Adresse des Admin-Servers. Für Proxys in Containern `0.0.0.0:9900` plus Token oder privates Netz. |
| `token` | leer | Wenn gesetzt, verlangen `/api/*` und `/_sds/check/*` den Header `X-SDS-Token` oder `Authorization: Bearer`. Die Snippets aus `sds integrate` tragen ihn ein. Die Web-Übersicht fragt ihn beim ersten Aufruf ab. |
| `public_url` | leer | Reserviert für Setups, in denen die Warteseite unter einer anderen Adresse liegt. |

Der Admin-Server spricht nur HTTP. TLS gehört vor einen Reverse-Proxy, falls die
Web-Übersicht von außerhalb des Hosts erreichbar sein soll.

### `[docker]`

Ohne `host` gilt `DOCKER_HOST`, sonst der lokale Socket. Docker Desktop (macOS) wird über
denselben Socket erreicht.

### `[discovery]`

`sds` lauscht auf Docker-Events und reagiert sofort auf neue, umbenannte oder gelöschte
Container. Der Intervall-Abgleich fängt verpasste Events ab (z. B. nach einem
Reconnect zum Daemon).

### `[log]`

`filter` folgt der Syntax von `RUST_LOG`; die Umgebungsvariable `SDS_LOG` übersteuert den
Wert. Mit `json = true` eignet sich die Ausgabe für Loki, Elastic oder journald-Parser.

### `[defaults]` und Gruppen-Einstellungen

| Feld | Bedeutung und Hinweise |
|---|---|
| `idle_timeout` | Zeit seit der letzten Anfrage, nach der gestoppt wird. Beim nativen Proxy zählen offene Verbindungen als Aktivität; bei Forward-Auth nur die Anfragen selbst (lange Downloads also mit Reserve einplanen). |
| `start_timeout` | Obergrenze für Start plus Health-Check. Danach gilt die Gruppe als `failed`; die nächste Anfrage nach 10 s löst einen neuen Versuch aus. |
| `stop_timeout` | Wird an `docker stop` übergeben. Datenbanken brauchen oft mehr als die Compose-Standardzeit von 10 s. |
| `start_grace` | Wartezeit, wenn kein anderer Health-Check greift. |
| `health` | `docker`: HEALTHCHECK des Containers (alle Mitglieder mit HEALTHCHECK müssen `healthy` sein; hat keines einen, gilt `start_grace`). `http://…`: GET bis 2xx/3xx. `tcp://host:port`: Verbindungsaufbau. `grace`: nur warten. Adressen gelten aus Sicht des `sds`-Prozesses. |
| `wake` | `hold`: Anfrage wird bis zur Bereitschaft gehalten, der Client merkt nur eine Verzögerung. `page`: sofort Warteseite mit automatischem Reload (für Browser-Anwendungen mit langen Startzeiten). |
| `hold_timeout` | Nach dieser Zeit bekommt eine gehaltene Anfrage eine 503 mit Warteseite. Muss kleiner sein als die Timeouts des Reverse-Proxys (die Snippets setzen diese passend). |
| `max_waiters` | Schutz vor Lastspitzen beim Aufwecken: mehr gleichzeitig wartende Anfragen werden sofort mit 503 beantwortet. |

### `[groups.<name>]`

Für Container, die keine Labels tragen können (fremde Images, manuell gestartete
Container). `containers` enthält Namen wie in `docker ps`. Fehlt ein Container, erscheint
die Gruppe trotzdem mit einer Warnung, damit das Problem sichtbar ist. Trägt ein
Container zusätzlich Labels, gewinnen die Labels.

Gruppennamen dürfen keine Leerzeichen und kein `/` enthalten, weil sie Teil der URL
`/_sds/check/<name>` sind.

## Änderungen anwenden

Nach einer Änderung an `sds.toml` den Dienst neu starten (`systemctl restart sds`). Ein
Reload per SIGHUP ist derzeit nicht implementiert; `sds` protokolliert den Hinweis.
Label-Änderungen an Containern brauchen keinen Neustart: `docker compose up -d` erzeugt
neue Container, die `sds` sofort erkennt.

Vor dem Neustart lohnt sich `sds check --config /etc/sds/sds.toml`: Es validiert die
Datei, prüft den Docker-Zugriff und listet alle Gruppen mit ihren effektiven Werten.
Exit-Code 1 bedeutet, dass es Warnungen gibt.
