# Fehlersuche

Erster Schritt immer: `sds check` (Konfiguration und Discovery) und `sds status --events`
(Zustand und letzte Ereignisse) oder die Web-Übersicht.

| Symptom | Ursache | Abhilfe |
|---|---|---|
| `sds check`: `docker daemon unreachable` | Socket nicht lesbar oder falscher `docker.host` | Benutzer in Gruppe `docker` oder als root; `DOCKER_HOST` prüfen |
| Gruppe fehlt in `sds status` | Kein `sds.enable=true`, oder Label fehlerhaft (siehe Warnung in `sds check`) | Labels prüfen, `docker compose up -d` erneut ausführen |
| Gruppe heißt anders als erwartet | Gruppenname = Compose-Projekt (Ordnername) | `sds.group` setzen oder `COMPOSE_PROJECT_NAME` |
| Warnung „sets different sds.* values“ | Mitglieder widersprechen sich | Gruppenweite Labels auf einen Container beschränken |
| Gruppe schläft nie ein | Anfragen kommen laufend (Monitoring, Health-Checks des Proxys, Crawler) | `sds status` zeigt `requests`; Health-Checks des Proxys auf einen Pfad legen, der am Forward-Auth vorbeigeht, oder Intervalle größer als `idle_timeout` wählen |
| Gruppe schläft nie ein (nativer Proxy) | Offene Keep-Alive-Verbindung | `sds status` zeigt `N open`; Client begrenzen |
| Gruppe wacht nicht auf, Browser zeigt 502 | Anfrage geht am Forward-Auth vorbei (Location ohne `auth_request`, falscher Pfad) | Proxy-Snippet vergleichen; `curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:9900/_sds/check/<gruppe>` muss 200 liefern und die Gruppe wecken |
| Zustand `failed`, „not ready after 90s“ | Health-Check greift nicht (falsche Adresse, Container ohne HEALTHCHECK aber `health=docker` und Start dauert länger als `start_grace`) | Adresse aus Sicht des `sds`-Prozesses prüfen; `start_grace`/`start_timeout` erhöhen; mit `curl` gegen die Health-URL testen |
| Zustand `failed`, „container … is exited“ | Mitglied stürzt beim Start ab | `docker logs <container>`; oft fehlende Abhängigkeit, Restart-Policy setzen |
| Zustand `failed`, „docker start …: 404“ | Container wurde neu erstellt (andere ID) | Discovery holt das innerhalb von Sekunden nach; sonst `sds check` |
| Warteseite erscheint, lädt aber ewig neu | Gruppe wird `running`, Proxy leitet trotzdem nicht weiter | Proxy-Cache oder `proxy_cache` für die Check-Location abschalten; nginx: `error_page`-Zeilen prüfen |
| nginx liefert 500 statt Warteseite | `?deny=403` fehlt oder `error_page 403` fehlt | Snippet neu erzeugen |
| Traefik liefert 500 | `sds` nicht erreichbar (kein Fail-open) | `systemctl status sds`; Adresse `host.docker.internal` plus `extra_hosts` prüfen |
| 503 „Busy“ | Mehr als `max_waiters` Anfragen warten gleichzeitig | Wert erhöhen oder `wake = page` nutzen |
| Anfragen hängen genau `hold_timeout` lang und enden in 503 | Startzeit größer als `hold_timeout` | `wake = page` für diese Gruppe oder `hold_timeout` erhöhen (Proxy-Timeout mitziehen) |
| Web-Übersicht „disconnected“ | Token gesetzt, SSE ohne Token nicht möglich | Seite neu laden, Token eingeben; die Seite pollt dann alle 2 s |
| `proxy not started: bind … address in use` | Port für nativen Proxy belegt | Container veröffentlicht den Port noch selbst; auf `127.0.0.1:<anderer Port>` umstellen |
| Container startet nach Host-Neustart von selbst | `restart: always` | Gewollt; `sds` erkennt `running` und legt nach `idle_timeout` wieder schlafen |

## Debug-Logging

```bash
SDS_LOG=debug sds run --headless
```

zeigt jede Discovery, jeden Health-Probe-Versuch und Docker-Events. Für den Dauerbetrieb
`info` belassen.

## Einen Zustand von Hand nachstellen

```bash
sds sleep shop                      # stoppt
curl -i http://127.0.0.1:9900/_sds/check/shop        # hält bis bereit, dann 200
curl -i -H 'Accept: text/html' 'http://127.0.0.1:9900/_sds/check/shop?mode=page'   # 503 + HTML
sds wake shop --wait                # Exit-Code 0, sobald gesund
```
