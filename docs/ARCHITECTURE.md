# Architecture de Dotmon

## Principes

1. **Séparation stricte** : la logique (core) ne connaît ni le noyau ni Slint ;
   les backends (sys) ne connaissent pas l'UI ; l'UI ne fait aucun appel système.
2. **Jamais d'action implicite** : aucun processus n'est terminé sans demande
   explicite *et* confirmation. Le moteur n'a pas de logique « auto-kill ».
3. **Double contrôle de protection** : le présentateur refuse avant d'ouvrir
   le dialogue ; le moteur revérifie contre son dernier échantillon juste
   avant d'envoyer le signal.
4. **Identité de processus = (PID, heure de démarrage)** : un PID recyclé entre
   la sélection et la confirmation est refusé (`ControlError::PidReused`).
5. **Pas de root** : les processus d'autres utilisateurs sont affichés mais
   protégés.
6. **Aucune valeur inventée** : une mesure illisible est `None` dans le
   modèle et « Indisponible » à l'écran ; le backend réel ne bascule jamais
   silencieusement vers la simulation.
7. **Phase 3 : fermeture réelle** (`docs/PHASE3.md`) : FERMER = SIGTERM,
   FORCER = SIGKILL, deux actions distinctes, jamais d'escalade automatique ;
   signaux par `pidfd`, confirmation liée à un signal et à des identités.

## Crates

```
┌──────────────────────── dotmon-app (binaire) ─────────────────────────┐
│                                                                        │
│  UI thread                                   engine thread             │
│  ┌──────────┐  callbacks   ┌───────────┐     ┌──────────────────────┐  │
│  │ .slint   │ ───────────▶ │ App       │ cmd │ Engine               │  │
│  │ (Theme,  │              │ Presenter │ ──▶ │  MetricsSource       │  │
│  │  Sys,Api)│ ◀─────────── │ bridge    │ ◀── │  ProcessController   │  │
│  └──────────┘  properties  └───────────┘ evt │  + garde protection  │  │
│                                              └──────────────────────┘  │
└────────────────────────────────────────────────────────────────────────┘
        │ uses                                     │ uses
        ▼                                          ▼
   dotmon-core  ◀──────────────────────────  dotmon-sys (mock | linux)
```

### `dotmon-core` — pur, entièrement testé

| Module       | Rôle |
|--------------|------|
| `model`      | `Snapshot`, `ProcessInfo`, `ProcessId`, stats CPU/RAM/GPU/disques/réseau |
| `history`    | séries bornées (système et par processus, purgées automatiquement) |
| `apps`       | regroupement processus → applications par arbre de processus ; classification APP / SERVICE / SESSION / SYSTÈME / NOYAU |
| `query`      | recherche (nom, commande, utilisateur, `pid:N`), filtres, tri stable |
| `protection` | politique : init, noyau, soi-même, composants de session (Hyprland, Quickshell, PipeWire, portails…), autres utilisateurs |
| `settings`   | réglages TOML, valeurs bornées, écriture atomique |
| `chart`      | chemins SVG des courbes, colonnes de points, échelles « rondes » |
| `source`     | traits `MetricsSource` et `ProcessController` |

**Pourquoi l'arbre et pas seulement les cgroups ?** Sur cette machine,
Hyprland est lancé sans uwsm : la plupart des applications partagent
`session-2.scope`. Une application = l'ancêtre le plus haut sous un « hôte
de session » (init, systemd utilisateur, Hyprland, qs…) + ses descendants.
Les cgroups servent à repérer les services (`*.service`).

### `dotmon-sys`

- `mock` : machine simulée déterministe (graine fixe), processus réalistes
  d'une session Hyprland + Quickshell, tâches `rustc` éphémères,
  contrôleur qui respecte PID + heure de démarrage et les droits.
- `linux` : backend réel, une seule instance vivant sur le thread moteur.
  - CPU global, mémoire, processus : `sysinfo` 0.40 (instance `System`
    conservée, processus sans `tasks`), complété par une lecture de
    `/proc/<pid>/stat` pour l'identité (heure de démarrage en ticks), le PPID,
    l'état, le nombre de threads, le drapeau thread noyau et le temps CPU ;
    cgroup lu une fois par processus.
  - CPU par processus : `linux/cpu.rs` (`CpuTracker`) calcule
    Δ(utime + stime) / (CLK_TCK × Δt × cœurs) avec le temps réel écoulé entre
    deux lectures du même processus ; sans lecture antérieure : `None` (« — »).
    Voir `docs/VALIDATION.md`, « Phase 2.1 ».
  - Température CPU : un seul capteur (`k10temp Tctl`…) rafraîchi.
  - Disques : `sysinfo::Disks` (statvfs) dédoublonné par périphérique ;
    débits depuis `/proc/diskstats`, totaux sur les disques physiques.
  - Réseau : `/proc/net/dev`, interfaces physiques actives uniquement.
  - GPU : NVML pour NVIDIA, sysfs amdgpu pour AMD, noms via `pci.ids` ;
    politique Actif / Repos / Veille pour ne jamais garder un GPU éveillé.
    La puissance d'un APU AMD (CPU compris) va dans `soc_power_w`, affichée
    « … W · SOC » ; `power_w` ne contient que la puissance du GPU seul.
  - Rescans périodiques : interfaces (10 éch.), montages (30), utilisateurs (60).
  - Débits calculés avec le temps écoulé réellement mesuré ; un compteur qui
    recule donne « indisponible », pas une valeur aberrante.
  - `linux/control.rs` : `LinuxController` (pidfd_open + vérification de
    l'heure de démarrage + pidfd_send_signal, pidfd conservé pour observer la
    sortie) derrière un trait `Kernel` (vrai noyau via `rustix`, faux noyau
    pour tester EPERM, identité non vérifiable, etc.).

### `dotmon-app`

- `engine` : thread d'échantillonnage. Reçoit `SetInterval`, `End{batch,…}`,
  `Shutdown`. Toute commande provoque un rafraîchissement immédiat. Après un
  signal délivré, il observe la cible (SIGTERM 5 s, SIGKILL 3 s) et émet
  `EndOutcome` (`Exited` / `StillRunning`) ; il n'envoie jamais de second
  signal de lui-même.
- `presenter` : machine d'état de l'UI (sélection, requête, demande en
  attente, réglages), indépendante de Slint et testée.
- `bridge` : présentateur → globals Slint ; modèles mis à jour en place.
- `outcomes` : regroupe les résultats d'un lot en messages français.
- `app` : câblage des callbacks, toasts, persistance des réglages.
- `ui/` : `theme.slint` (jetons + globals partagés), `components/widgets.slint`,
  `pages/{dashboard,processes,settings}.slint`, `main.slint`.

## Flux « terminer une application »

```
clic FORCER ─▶ Presenter::request_end(Force) ─▶ core::actions::plan_end
                 ├─ protégé / ambigu / contient Dotmon ? ─▶ toast, aucun dialogue
                 └─ sinon ─▶ PendingEnd { lot, signal, identités } ─▶ dialogue
CONFIRMER ─▶ Presenter::confirm() (consommé une seule fois)
           ─▶ Engine: Command::End { lot, cible, signal } par identité
           ─▶ guarded_end : existe ? même starttime ? protégé ?
           ─▶ LinuxController::end : pidfd_open, /proc/<pid>/stat, pidfd_send_signal
           ─▶ Event::EndResult (signal délivré ou refus précis)
           ─▶ surveillance du pidfd ─▶ Event::EndOutcome (terminé / toujours actif)
```

Fermer (SIGTERM) une application vise son processus principal seul ; forcer
(SIGKILL) vise le processus principal et ses descendants vérifiés, listés dans
le plan. Voir `docs/PHASE3.md` pour les refus de groupe. Si la cible disparaît
pendant que le dialogue est ouvert, la demande est annulée.

## Rendu, performances

- Rendu logiciel Slint par défaut : ≈3 % d'un cœur et ≈32 Mio RSS (release,
  1 Hz, fenêtre 1270×770). Le rendu OpenGL (femtovg) coûte le même CPU mais
  ≈140 Mio (pilote NVIDIA chargé).
- Les animations sont courtes (140–260 ms) ; l'indicateur LIVE clignote sans
  fondu (un fondu par seconde faisait monter le CPU à 12 %).
- Mise en page adaptative : 3 colonnes / 2 colonnes défilantes / 1 colonne,
  liste compacte et détail superposé sous 1100 px (tuiles Hyprland).

## Tests

- `dotmon-core` : 72 tests unitaires (dont `actions` : planification des fermetures).
- `dotmon-sys` : 65 tests (simulation, parseurs `/proc` et `/sys`, politique
  GPU, tests sur la machine réelle comparés à `/proc`, faux noyau pour les
  erreurs de fermeture, et fermeture de vrais processus `sleep` créés par le test).
- `dotmon-app` : 49 tests (moteur, présentateur, résultats, et moteur sur le
  backend Linux réel avec des `sleep` jetables), dont `ui_tests::headless_ui_flow` qui rend la vraie
  fenêtre avec le moteur de rendu logiciel sur une plateforme factice et
  parcourt les callbacks réels (sélection, confirmation, protection, recherche,
  tri, réglages, fenêtres étroites). Les images sont écrites dans
  `target/shots/*.ppm`.

Capture de la vraie fenêtre Wayland (dev) :
`DOTMON_SNAPSHOT=/tmp/x.ppm DOTMON_PAGE=1 DOTMON_SELECT=firefox dotmon`.
