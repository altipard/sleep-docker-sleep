# Labels und Gruppen

Container beschreiben selbst, wie `sds` mit ihnen umgehen soll. Projektteams müssen
dafür nur Labels ins Compose-File schreiben; eine zentrale Liste ist nicht nötig.

## Gruppenbildung

Eine Gruppe ist die Einheit, die gemeinsam schläft und aufwacht. Der Name ergibt sich aus:

1. `sds.group`, falls gesetzt,
2. sonst dem Compose-Projekt (`com.docker.compose.project`, in der Regel der Ordnername),
3. sonst dem Containernamen.

Alle Container mit `sds.enable=true` und demselben Gruppennamen bilden eine Gruppe,
auch über Compose-Projekte hinweg. Der Name muss zur URL passen: keine Leerzeichen, kein
`/`.

Typische Muster:

| Situation | Labels |
|---|---|
| Host-nginx vor dem Projekt, alles darf schlafen | `sds.enable=true` auf allen Services |
| Projekt bringt eigenen Caddy/Traefik mit | `sds.enable=true` auf App und DB, `sds.keep_awake=true` auf dem Proxy |
| Zwei Projekte teilen sich eine Datenbank | `sds.group=shared-db` auf allen Beteiligten |
| Nur die App soll schlafen, die DB läuft durch | `sds.enable=true` nur auf der App |

Beim Aufwecken startet `sds` alle Mitglieder in alphabetischer Reihenfolge und wartet dann
auf den Health-Check. Compose-Abhängigkeiten (`depends_on`) werden nicht ausgewertet;
Anwendungen, die eine noch nicht bereite Datenbank nicht verkraften, brauchen eine
Restart-Policy (`restart: unless-stopped`) oder einen HEALTHCHECK, auf den `sds` wartet.

## Referenz

| Label | Werte | Beschreibung |
|---|---|---|
| `sds.enable` | `true`/`false` | Container wird Mitglied seiner Gruppe. Ohne dieses Label ignoriert `sds` den Container. |
| `sds.keep_awake` | `true` | Container wird nie von `sds` gestoppt. Dokumentiert Absicht, z. B. beim projekteigenen Proxy. |
| `sds.group` | Name | Explizite Gruppe statt Compose-Projekt. |
| `sds.idle_timeout` | Dauer | Siehe [Konfiguration](konfiguration.md#defaults-und-gruppen-einstellungen). |
| `sds.start_timeout` | Dauer | |
| `sds.stop_timeout` | Dauer | |
| `sds.start_grace` | Dauer | |
| `sds.health` | `docker`, `grace`, `http://…`, `tcp://host:port` | |
| `sds.wake` | `hold`, `page` | |
| `sds.hold_timeout` | Dauer | |
| `sds.max_waiters` | Zahl | |
| `sds.proxy.listen` | Adresse | Nativer Proxy, siehe [eigenes Kapitel](nativer-proxy.md). |
| `sds.proxy.upstream` | `host:port` | |
| `sds.proxy.mode` | `http`, `tcp` | |

Unbekannte `sds.*`-Labels sind Fehler: der Container wird ignoriert und `sds check`
nennt das Label. Das verhindert, dass ein Tippfehler wie `sds.idle_timout` unbemerkt
den Standardwert aktiviert.

Gruppenweite Einstellungen (`idle_timeout`, `health`, …) stehen sinnvollerweise auf einem
Container. Stehen sie auf mehreren und widersprechen sich, gewinnt der alphabetisch erste
Containername; die Web-Übersicht zeigt eine Warnung.

## Health-Check wählen

| Ziel | Empfehlung |
|---|---|
| Image hat einen brauchbaren HEALTHCHECK | `sds.health=docker` (Standard). `sds` wartet, bis alle Mitglieder mit HEALTHCHECK `healthy` melden. |
| Kein HEALTHCHECK, HTTP-Endpunkt vorhanden | `sds.health=http://127.0.0.1:18080/healthz` (Adresse aus Sicht des `sds`-Prozesses). |
| TCP-Dienst (Datenbank, SMTP) | `sds.health=tcp://127.0.0.1:5432` |
| Nichts davon | `sds.health=grace` und `sds.start_grace` großzügig setzen. |

Stürzt ein Mitglied während des Startens ab (`exited`), bricht `sds` sofort ab und meldet
die Gruppe als `failed`, statt bis `start_timeout` zu warten.

## Beispiel: Compose-Projekt mit eigenem Caddy

```yaml
services:
  caddy:
    image: caddy:2-alpine
    ports: ["80:80", "443:443"]
    extra_hosts: ["host.docker.internal:host-gateway"]
    labels:
      sds.keep_awake: "true"
  app:
    image: registry.example.com/shop:1.4.2
    labels:
      sds.enable: "true"
      sds.group: "shop"
      sds.idle_timeout: "20m"
      sds.health: "docker"
  db:
    image: postgres:16
    labels:
      sds.enable: "true"
      sds.group: "shop"
      sds.stop_timeout: "60s"
```

Nach `docker compose up -d` erscheint die Gruppe `shop` in `sds status`. Caddy läuft
durch und fragt `sds` über `host.docker.internal:9900` an, siehe
[Reverse-Proxy-Anbindung](reverse-proxy.md).

## Wechselwirkung mit Restart-Policies

`docker stop` durch `sds` setzt bei `restart: always` und `restart: unless-stopped` den
Container nicht erneut in Gang; Docker merkt sich den manuellen Stopp. Zwei Eigenheiten:

- Nach einem Neustart des Docker-Daemons (oder des Hosts) startet `restart: always`
  gestoppte Container wieder. `sds` erkennt das und führt die Gruppe als `running`; sie
  schläft nach `idle_timeout` erneut ein.
- Stürzt eine laufende Anwendung ab und Docker startet sie per Restart-Policy neu, sieht
  `sds` kurz `stopped`, dann wieder `running`. Das ist beabsichtigt und sichtbar im
  Ereignisprotokoll.
