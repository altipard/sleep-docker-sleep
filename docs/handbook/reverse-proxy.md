# Reverse-Proxy-Anbindung

`sds` nutzt den Forward-Auth-Mechanismus, den nginx (`auth_request`), Caddy
(`forward_auth`) und Traefik (`forwardAuth`) mitbringen. Der Proxy schickt vor jeder
Anfrage einen kleinen Request an `sds`; die Antwort entscheidet, ob die Anfrage
weitergeleitet wird.

`sds integrate <proxy> <gruppe>` erzeugt das passende Snippet. Die proxy-spezifischen
Eigenheiten stecken im Generator; dieses Kapitel erklärt sie, damit Anpassungen möglich
sind.

## Endpunkte

| Pfad | Zweck |
|---|---|
| `GET /_sds/check/<gruppe>` | Forward-Auth. `200`, wenn die Gruppe läuft. Sonst wird sie geweckt; je nach `wake` hält `sds` die Anfrage oder antwortet mit Status `503` (bzw. `?deny=…`) und Warteseite. |
| `GET /_sds/wait/<gruppe>` | Warteseite ohne Auth-Semantik, für Proxys, die den Body der Check-Antwort nicht durchreichen (nginx). |

Parameter für `/_sds/check`:

| Parameter | Wirkung |
|---|---|
| `?mode=hold` / `?mode=page` | übersteuert `wake` der Gruppe |
| `?deny=403` | Statuscode für „nicht bereit“ (Standard `503`) |

Die Check-Antwort trägt immer den Header `X-SDS-State` (`running`, `starting`, `failed`,
`overloaded`, `unknown-group`). Browser (`Accept: text/html`) bekommen die HTML-Warteseite
mit `Refresh`-Header, andere Clients JSON mit `Retry-After`.

Unbekannte Gruppen beantwortet `sds` mit `200` und einer Warnung im Log (Fail-open): ein
Tippfehler in der Proxy-Konfiguration darf keine Seite lahmlegen.

## Netzwerkpfad zu `sds`

| `sds` läuft … | Proxy läuft … | Adresse im Snippet |
|---|---|---|
| auf dem Host | auf dem Host (nginx) | `http://127.0.0.1:9900` |
| auf dem Host | im Container (Caddy/Traefik) | `http://host.docker.internal:9900`; dafür `extra_hosts: ["host.docker.internal:host-gateway"]` im Proxy-Service und `admin.bind = "0.0.0.0:9900"` oder die Docker-Bridge-Adresse plus Token |
| im Container | im Container, gleiches Netz | `http://sds:9900` |

Sobald `admin.bind` nicht mehr `127.0.0.1` ist, einen `admin.token` setzen. Die Snippets
senden ihn als `X-SDS-Token`.

## Timeouts

Bei `wake = hold` hält `sds` die Check-Anfrage bis zu `hold_timeout` (Standard 60 s). Der
Proxy darf diese Verbindung nicht früher abbrechen. Die Snippets setzen den jeweiligen
Lese-Timeout auf `hold_timeout + 5 s`. Wer `hold_timeout` erhöht, muss den Proxy-Wert
mitziehen; `sds integrate --hold-timeout <sek>` erzeugt das Snippet entsprechend.

## nginx

```bash
sds integrate nginx shop --host shop.example.com --upstream http://127.0.0.1:18080
```

Besonderheiten von `auth_request`:

- Es akzeptiert nur `2xx` (erlauben) sowie `401`/`403` (ablehnen); alles andere wird zu
  `500`. Das Snippet ruft deshalb `/_sds/check/shop?deny=403` auf und wandelt `403` per
  `error_page 403 = @sds_wait` in die Warteseite um (`/_sds/wait/shop`).
- Der Body der Subrequest-Antwort geht verloren, daher der Umweg über `@sds_wait`.
- `proxy_intercept_errors` bleibt `off`: Ein `403` der Anwendung selbst wird nicht
  angefasst, nur das von `auth_request` erzeugte.
- **Fail-open:** Ist `sds` nicht erreichbar, erzeugt nginx intern `500`/`502`;
  `error_page 500 502 = @sds_bypass` leitet dann direkt an die Anwendung weiter. Diese
  Zeile fängt auch andere nginx-interne 500er; wer das nicht möchte, entfernt sie und
  nimmt Fehler während eines `sds`-Ausfalls in Kauf.
- `map $http_upgrade $connection_upgrade` einmal im `http {}`-Kontext anlegen, damit
  WebSockets funktionieren.
- `proxy_read_timeout` im Check-Location muss `hold_timeout` übersteigen.

## Caddy

```bash
sds integrate caddy shop --host shop.example.com --upstream http://app:80 \
    --sds-url http://host.docker.internal:9900
```

- `forward_auth` reicht Nicht-2xx-Antworten samt Body an den Client durch; die Warteseite
  kommt direkt von `sds`, `?deny` ist nicht nötig.
- `copy_headers X-SDS-State` macht den Zustand in Caddy-Logs sichtbar.
- **Kein Fail-open:** `forward_auth` ist intern ein `reverse_proxy`; ist `sds` nicht
  erreichbar, entsteht ein Transportfehler, den `handle_response` nicht abfängt, und der
  Client sieht `502`. Ein `handle_errors`-Block könnte in dem Fall an die Anwendung
  weiterleiten, kann aber einen Ausfall von `sds` nicht von einem Ausfall der Anwendung
  unterscheiden; deshalb erzeugt das Snippet ihn nicht. Stattdessen `sds` als
  überwachten Dienst betreiben.
- `response_header_timeout` muss `hold_timeout` übersteigen.

## Traefik

```bash
sds integrate traefik shop --host shop.example.com --upstream http://app:80 \
    --sds-url http://host.docker.internal:9900
```

Das Snippet enthält die Variante als dynamische Datei und als Container-Labels.

- `forwardAuth` reicht Nicht-2xx-Antworten an den Client durch, Warteseite inklusive.
- `authResponseHeaders: [X-SDS-State]` kopiert den Zustand in die Antwort.
- **Kein Fail-open:** Traefik hat für `forwardAuth` keinen Bypass. Ist `sds` nicht
  erreichbar, antwortet Traefik mit `500`. `sds` deshalb als überwachten Dienst
  (`Restart=always`) betreiben; ein Neustart dauert unter einer Sekunde.
- `forwardAuthTimeout` (Traefik ≥ 3.3) muss `hold_timeout` übersteigen. Ältere Versionen
  nutzen den globalen `serversTransport`-Timeout.
- Token: Traefik kann keine festen Header an den Auth-Dienst anhängen. Entweder `sds`
  ohne Token in einem privaten Docker-Netz betreiben (empfohlen) oder den Token über
  `headers`-Middleware vorlagern.

## Prüfen

```bash
sds sleep shop
curl -sI https://shop.example.com/ | head -1     # 503 + Warteseite oder nach Hold 200
sds status                                      # Gruppe läuft wieder
```

In der Web-Übersicht (`http://127.0.0.1:9900/`) erscheinen die Ereignisse „waking“ und
„running“ mit Zeitstempel.
