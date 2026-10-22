# Nativer Proxy

Für Stacks ohne Reverse-Proxy und für reine TCP-Dienste kann `sds` selbst den
öffentlichen Port halten. Das ist der einzige Betriebsmodus, in dem `sds` in der
Datenebene steht; entsprechend ist er für Sonderfälle gedacht, nicht als Ersatz für
nginx, Caddy oder Traefik.

## Port-Umstellung

Der Container darf seinen Port nicht mehr selbst öffentlich veröffentlichen, sonst
kollidieren beide. Muster:

```yaml
services:
  app:
    ports:
      - "127.0.0.1:18080:80"        # nur lokal
    labels:
      sds.enable: "true"
      sds.group: "demo"
      sds.health: "http://127.0.0.1:18080/"
      sds.proxy.listen: "0.0.0.0:8080"
      sds.proxy.upstream: "127.0.0.1:18080"
      sds.proxy.mode: "http"
```

`sds` lauscht auf `:8080`, leitet an `127.0.0.1:18080` weiter und zählt jede Verbindung
als Aktivität. Dasselbe lässt sich in `sds.toml` unter `[groups.<name>.proxy]` eintragen.

Ein belegter Port wird beim Start der Gruppe gemeldet (Ereignis „proxy not started“), die
Gruppe selbst funktioniert weiter über Forward-Auth.

## Modi

| Modus | Verhalten |
|---|---|
| `http` | Versteht Anfragen. Schlafende Gruppe: je nach `wake` halten oder Warteseite (503). WebSocket-Upgrades werden durchgereicht. Bodies werden gestreamt, nicht gepuffert. Header-Lese-Timeout 30 s. Setzt `X-Forwarded-For` und `X-Forwarded-Proto`. |
| `tcp` | Opaker Bytestrom (Datenbanken, SMTP, SSH). Verbindung wird angenommen, Gruppe geweckt, nach Bereitschaft wird zum Upstream verbunden und beidseitig kopiert. Vor der Bereitschaft fließen keine Bytes; `wake = page` hat keine Wirkung. |

## Idle-Erkennung

Im Gegensatz zu Forward-Auth zählen hier **offene Verbindungen** als Aktivität. Eine
Gruppe schläft erst, wenn keine Verbindung mehr offen ist und `idle_timeout` seit der
letzten abgelaufen ist. Lange Downloads und WebSockets halten die Gruppe also wach.
Kehrseite: Clients mit dauerhaften Keep-Alive-Verbindungen (Monitoring, Load-Balancer)
verhindern das Einschlafen. Solche Verbindungen beim Client begrenzen oder einen
Reverse-Proxy mit Forward-Auth verwenden.

## Grenzen

- Kein TLS. Für HTTPS einen Reverse-Proxy davorsetzen, der dann aber gleich Forward-Auth
  nutzen kann.
- HTTP/1.1 zum Upstream; HTTP/2 vom Client wird nicht angeboten.
- Kein Load-Balancing, ein Upstream pro Gruppe.
- `sds` ist in diesem Modus Single Point of Failure für die Gruppe. Dienst überwachen.
