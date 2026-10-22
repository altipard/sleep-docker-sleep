# Grenzen und bekannte Eigenheiten

Bewusste Entscheidungen und offene Punkte der aktuellen Version. Wer eine davon
braucht, sollte sie als Issue anlegen, statt sie in Proxy-Konfiguration zu umgehen.

## Fail-open nur bei nginx

nginx kann einen nicht erreichbaren `sds` per `error_page` umgehen. Caddy und Traefik
haben für Forward-Auth keinen sauberen Bypass: Ist `sds` nicht erreichbar, sehen Clients
`502` (Caddy) bzw. `500` (Traefik). Die mitgelieferten Units starten `sds` nach einem
Absturz innerhalb von zwei Sekunden neu; `sds` selbst braucht unter einer Sekunde bis zur
Betriebsbereitschaft und liest den Ist-Zustand aus Docker.

## Idle-Erkennung bei Forward-Auth zählt Anfragen, nicht Verbindungen

Der Proxy meldet `sds` den Beginn einer Anfrage, nicht ihr Ende. Ein Download, der
länger dauert als `idle_timeout`, kann unterbrochen werden. In der Praxis sind
`idle_timeout`-Werte von 10 Minuten und mehr unkritisch; wo es darauf ankommt, hilft der
native Proxy, der offene Verbindungen zählt.

## Kein Config-Reload

Änderungen an `sds.toml` brauchen einen Neustart des Dienstes. Label-Änderungen an
Containern wirken sofort. SIGHUP wird entgegengenommen und protokolliert, aber nicht
ausgewertet.

## Startreihenfolge innerhalb einer Gruppe

Mitglieder werden alphabetisch gestartet; `depends_on` aus Compose wird nicht gelesen.
Anwendungen sollten einen Verbindungsaufbau wiederholen oder eine Restart-Policy haben.
Gestoppt wird parallel.

## Compose-Abhängigkeiten über Gruppen hinweg

`sds` weiß nicht, dass Gruppe A Gruppe B braucht. Beide werden unabhängig geweckt und
schlafen gelegt. Gemeinsame Abhängigkeiten gehören in dieselbe Gruppe (`sds.group`) oder
bleiben wach (`sds.keep_awake`).

## Kein TLS am Admin-Server

Admin-API und Web-Übersicht sprechen HTTP. Standardmäßig lauschen sie nur auf
`127.0.0.1`. Für Zugriff von außen einen Reverse-Proxy mit TLS davorsetzen.

## Nur ein Docker-Daemon pro Instanz

Swarm, Kubernetes und entfernte Hosts über mehrere Sockets werden nicht unterstützt. Pro
Host läuft ein `sds`.

## Health-Checks aus Sicht des `sds`-Prozesses

`http://` und `tcp://` werden von dort ausgeführt, wo `sds` läuft. Auf dem Host ist das
der veröffentlichte Port (`127.0.0.1:18080`), im Container der Dienstname im Docker-Netz.
Auf macOS sind Container-IPs vom Host nicht erreichbar; dort immer den veröffentlichten
Port verwenden oder `health = docker`.

## Erster Aufruf ist langsam

Das ist das Prinzip: Der erste Aufrufer wartet die Startzeit der Container ab. Wer das
für bestimmte Clients vermeiden will (API-Konsumenten mit kurzen Timeouts), nutzt
`wake = page` für Browser und hält `idle_timeout` für solche Gruppen hoch.

## Unbekannte Gruppen werden durchgelassen

Ein Tippfehler im Proxy-Snippet führt nicht zu einem Ausfall, sondern zu einer Warnung
im Log und `X-SDS-State: unknown-group`. Nach Änderungen an Gruppennamen die Logs
beobachten.
