# sleep-docker-sleep (`sds`) – Handbuch

`sds` stoppt Docker-Container, die eine Weile keine Anfragen bekommen haben, und startet
sie wieder, sobald die nächste Anfrage eintrifft. Der erste Aufrufer wartet kurz oder
bekommt eine Warteseite, danach läuft alles wie gewohnt. Ziel: Ressourcen auf Hosts mit
vielen selten genutzten Stacks freihalten, ohne dass jemand manuell etwas starten muss.

Dieses Handbuch richtet sich an Administratoren, die `sds` einrichten und betreiben.
Entwickler finden am Ende ein Kapitel zur [Architektur](architektur.md).

## Wie es funktioniert

```text
                       1. Anfrage                      3. Antwort "200, weiter"
  Browser ──────────▶ Reverse-Proxy ──── 2. forward-auth ────▶ sds ─────┐
                      (nginx/Caddy/      GET /_sds/check/shop            │ startet bei
                       Traefik)                                          │ Bedarf die
                          │ 4. Anfrage an die App                        ▼ Container
                          └────────────────────────────────▶ Container-Gruppe "shop"
                                                             (app, db, …)
```

1. Der Reverse-Proxy fragt vor jeder Anfrage bei `sds` an (Forward-Auth, ein Request von
   wenigen Mikrosekunden, solange die Gruppe läuft).
2. Läuft die Gruppe, antwortet `sds` sofort mit `200` und merkt sich die Aktivität.
3. Schläft die Gruppe, startet `sds` die Container und hält die Anfrage, bis die Gruppe
   gesund ist (`wake = hold`), oder liefert sofort eine Warteseite, die sich selbst neu
   lädt (`wake = page`).
4. Kommt über `idle_timeout` hinweg keine Anfrage, stoppt `sds` die Gruppe.

`sds` ist damit **Control-Plane**: die eigentlichen Daten laufen weiterhin über den
vorhandenen Reverse-Proxy. Fällt `sds` aus, läuft der Verkehr bei nginx unverändert
weiter (Fail-open); nur das Schlafenlegen pausiert. Caddy und Traefik kennen für
Forward-Auth keinen Bypass, dort überbrückt ein überwachter Dienst mit automatischem
Neustart die Lücke (siehe [Grenzen](grenzen.md)).

Für Stacks ganz ohne Reverse-Proxy und für reine TCP-Dienste gibt es zusätzlich einen
[nativen Proxy](nativer-proxy.md), bei dem `sds` selbst den öffentlichen Port hält.

## Begriffe

| Begriff | Bedeutung |
|---|---|
| **Gruppe** | Menge von Containern, die gemeinsam schlafen und aufwachen. Standard: ein Compose-Projekt. |
| **Mitglied** | Ein Container in einer Gruppe (`sds.enable=true`). |
| **keep_awake** | Container im selben Projekt, den `sds` nie stoppt (z. B. der projekteigene Caddy). |
| **Forward-Auth** | Mechanismus, mit dem der Reverse-Proxy vor jeder Anfrage bei `sds` nachfragt. |
| **hold / page** | Verhalten beim Aufwecken: Anfrage halten bzw. Warteseite liefern. |
| **Health** | Kriterium, ab wann eine gestartete Gruppe Verkehr bekommt (Docker-HEALTHCHECK, HTTP, TCP oder Wartezeit). |

## Was `sds` nicht ist

- Kein Ersatz für einen Reverse-Proxy in Produktion. Der native Proxy ist für Sonderfälle.
- Kein Orchestrator: `sds` startet und stoppt vorhandene Container, es erzeugt keine.
- Kein Autoscaler: es gibt nur „an“ und „aus“, keine Replikas.
