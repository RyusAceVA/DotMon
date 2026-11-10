# Validation des métriques réelles (phase 2)

Machine de développement : CachyOS, noyau 7.3.3-1-cachyos, AMD Ryzen AI 7 350 (16 threads),
31,9 Gio de RAM, zram, NVIDIA RTX 5070 Laptop + iGPU AMD Radeon 860M,
deux NVMe (Linux sur `nvme1n1`, Windows non monté sur `nvme0n1`), `eno1` + `wlan0`.

Protocole complet à suivre sur la machine : [`PROTOCOLE.md`](PROTOCOLE.md).
Comparaison automatique avec verdicts : `scripts/compare.sh [N]` (ne réveille
jamais le GPU NVIDIA ; `--nvidia-smi` ne l'interroge que s'il est déjà allumé).
Sortie brute du backend : `cargo run --release -p dotmon-sys --example probe -- 5`.

## Résultats (fenêtres de ~1 s, décalées de quelques dizaines de ms)

| Mesure | Outil indépendant | Dotmon | Écart |
|---|---|---|---|
| CPU global | `/proc/stat` 1,2 % | 1,7 % | fenêtres décalées |
| RAM utilisée | `free` 6 891 252 Kio | 6 892 344 Kio | 1 Mio (lectures successives) |
| RAM disponible | `free` 25 050 640 Kio | 25 049 548 Kio | idem |
| buff/cache | `free` 18 361 680 Kio | 18 361 680 Kio | aucun |
| Swap | `free` 2 492 / 31 941 628 Kio | identique | aucun |
| Processus | `ps -e` 404 | 401 | `ps`, `wc`, le script lui-même |
| Threads | `ps -eLf` 1 231 | 1 229 | idem |
| RSS Hyprland | `ps` 238 636 Kio | 238 636 Kio | aucun |
| CPU par processus (phase 2) | `top` 6,9 / 5,0 / 5,0 / 4,0 % | 8 / 6 / 6 / 4 % (× 16 cœurs) | fenêtres et granularité ; voir « Phase 2.1 » pour le biais corrigé |
| Lecture nvme1n1 | `/proc/diskstats` 61 235 o/s | 60 734 o/s | fenêtres décalées |
| Réseau (totaux) | `/proc/net/dev` | identiques à l'octet | aucun |
| `/` utilisé | `df` 33 192 540 Kio | 33 961 536 Kio | voir ci-dessous |
| GPU NVIDIA | `nvidia-smi` : 8 151 Mio, 41 °C, 7,9 W, 1 % | 8 151 Mio, 41 °C, 8,7–9,7 W, 0 % | puissance instantanée |
| GPU NVIDIA VRAM utilisée | `nvidia-smi` 2 Mio | 1,3 Mio | arrondi au Mio de nvidia-smi |

## Différences normales entre méthodes

- **CPU par processus** : Dotmon affiche le pourcentage **de la machine
  entière** (0–100 %, comme le CPU global). `top` et `ps` affichent le
  pourcentage **d'un cœur** (jusqu'à 1600 % ici) : multipliez la valeur de
  Dotmon par 16 pour comparer. `ps %CPU` est en plus une moyenne sur toute la
  vie du processus, pas sur la dernière seconde. Pour un processus né depuis
  la mesure précédente, Dotmon affiche « — » : `top` affiche une valeur dès
  la première image.
- **RAM utilisée** = total − disponible (`MemAvailable`), comme `free`
  (procps-ng 4). D'anciennes versions de `free`, et `htop`, calculent
  autrement (total − libre − buffers − cache).
- **Cache** = Buffers + Cached + SReclaimable, la colonne `tamp/cache` de `free`.
- **Espace disque** : Dotmon affiche « utilisé » = total − disponible. `df`
  calcule total − libre, sans les blocs réservés que btrfs ne rend pas
  disponibles (~750 Mio ici). Dotmon est donc plus haut que `df` de cette
  réserve.
- **Une ligne par périphérique** : les 7 sous-volumes btrfs montés depuis
  `nvme1n1p2` apparaissent une seule fois, sous le point de montage le plus
  court (`/`).
- **Débits disque totaux** : uniquement les disques physiques entiers
  (`/sys/block` hors `virtual`) : pas de double comptage partition + disque,
  et zram, loop et device-mapper sont exclus. Les débits par ligne sont ceux
  de la partition montée.
- **Réseau** : uniquement les interfaces physiques actives (`device` présent,
  `operstate = up`). `lo`, les ponts (`virbr0`), veth, tun et wireguard sont
  exclus : leur trafic passe aussi par l'interface physique.
- **E/S par processus** : `/proc/<pid>/io` n'est lisible que pour vos propres
  processus. Pour root et les autres utilisateurs, Dotmon affiche « — » /
  « Indisponible », jamais 0.
- **Nombre de processus** : les threads ne sont pas comptés comme processus
  (sysinfo sans `tasks`), comme `ps -e`.

## GPU et économie d'énergie

Constat mesuré : interroger NVML chaque seconde **empêchait la RTX de se
mettre en veille** (restée active 45 s sur 45 ; sans Dotmon elle se rendort
~19 s après le dernier client). Politique retenue :

1. GPU en veille (`power/runtime_status = suspended`) : jamais interrogé,
   affiché « VEILLE ».
2. GPU à gestion d'énergie dynamique (`power/control = auto`), mesuré
   inactif (0 % et aucun processus) : plus interrogé pendant 30 s, affiché
   « REPOS », ce qui laisse le pilote l'éteindre.
3. GPU utilisé : mesuré à chaque échantillon (il reste allumé de toute façon).

Vérification : après un réveil par `nvidia-smi`, une mesure, 18 échantillons
« REPOS », puis la carte passe en veille et y reste (26 échantillons « VEILLE »).
L'iGPU AMD (`power/control = on`, il pilote l'écran) est lu à chaque
échantillon via sysfs.

## Coût de Dotmon (release, rendu logiciel, 1 Hz)

- Collecte : 11 ms par échantillon, ~25 ms quand NVML est interrogé
  (initialisation et fermeture de NVML à chaque mesure).
- Processus complet : 3,5 % d'un cœur, 29 Mio de RSS, stable entre 10 s et
  60 s (pas de dérive), 6 threads.
- Initialisation du backend : 80–90 ms (avant l’ouverture de la fenêtre).

## Phase 2.1 : corrections des écarts confirmés

### CPU par processus : conventions comparées

| | Formule | Dénominateur |
|---|---|---|
| `top` (procps-ng) | Δ(utime + stime) / (CLK_TCK × Δt) × 100, en % d'**un** cœur (`I` : ÷ nombre de cœurs) | temps réel écoulé |
| sysinfo 0.40 (avant) | Δ(utime + stime) / (Δ somme de `/proc/stat` ÷ nombre de cœurs) × 100 | croissance des compteurs de `/proc/stat` |
| Dotmon (maintenant) | Δ(utime + stime) / (CLK_TCK × Δt × cœurs logiques) × 100, plafonné à 100 | temps réel écoulé, **propre à chaque processus** |

Mesure sur cette machine (fenêtres de 8 s, 16 cœurs) : les compteurs de
`/proc/stat` avancent à **97,7 % du temps réel au repos et 98,0–98,1 % sous
charge** (de 95,1 % à 100 % selon le cœur). C'est la comptabilité du noyau,
pas une erreur de lecture ; la cause exacte n'a pas été recherchée. Un
processus occupant un cœur à 100 % (99,9 % de l'horloge murale) était donc
rapporté à 102,7 % par sysinfo. Dotmon n'utilise plus ces compteurs pour
les processus : il lit lui-même `utime` + `stime` dans `/proc/<pid>/stat`
(déjà lu pour l'identité du processus : aucune lecture de plus) et mesure
Δt avec l'horloge monotone entre deux lectures du même processus. Le CPU
global reste celui de `/proc/stat`, comme `top`.

Un processus sans lecture antérieure (né depuis le dernier échantillon, ou
même PID réutilisé avec une autre heure de démarrage) n'a pas de mesure
fiable : `cpu = None`, affiché « — », exclu de l'historique et du tri
(placé après toute valeur mesurée). Pour une application, la valeur est la
somme de ses processus mesurés ; « — » si aucun ne l'est. Les processus déjà
présents au démarrage sont mesurés dès le premier échantillon (une lecture
de référence est prise à l'initialisation).

Mesures après correction (`scripts/compare.sh 8`, 4 cœurs chargés) :

| | Avant | Après |
|---|---|---|
| Biais moyen des processus ≥ 50 % d'un cœur | +2,1 à +3,6 % (4 fenêtres sur 4 hors tolérance) | −0,2 à +0,5 % (0 fenêtre hors tolérance sur 15) |
| `yes` à 100 % d'un cœur (× 16) | 100,7–102,5 % | 97,9–100,8 % (top : 98,2–100 %) |
| Processus né pendant la fenêtre | 0 % | « — » (top : 100 %), puis la valeur à l'échantillon suivant |

### Puissance du GPU AMD

Le noyau documente `power1_average` d'amdgpu comme la puissance « utilisée
par le SoC » et précise que « sur les APU, elle inclut le CPU »
(<https://docs.kernel.org/7.0/gpu/amdgpu/thermal.html>). Sur cette machine
l'étiquette est `PPT` (mesuré : 45–55 W sous charge CPU, bien au-dessus d'un
iGPU Radeon 860M). Un APU est reconnu à la tension Northbridge (`in1_*`,
« APUs only » dans la même documentation). Le modèle distingue donc
`power_w` (GPU seul : NVML, ou carte AMD discrète) et `soc_power_w` (puce
entière). L'écran affiche `54.5 W · SOC` pour la seconde, jamais `54.5 W`.
La température « edge » d'amdgpu est celle du GPU : inchangée.

## Tests automatisés

- `dotmon-sys::linux::procfs` : parseurs sur données figées (`stat` avec
  parenthèses dans le nom, threads noyau, `meminfo`, `diskstats`, `net/dev`,
  cgroup, remise à zéro des compteurs, `pci.ids`, arborescence sysfs factice).
- `dotmon-sys::linux::gpu` : GPU en veille jamais interrogé, cycle
  Actif → Repos → Veille → Actif, GPU toujours allumé interrogé à chaque fois.
- `dotmon-sys::linux::tests` (machine réelle) : CPU comparé à `/proc/stat`,
  nombre de cœurs, RAM comparée à `/proc/meminfo`, table des processus
  comparée à `/proc`, exactitude du processus de test lui-même (PPID, heure
  de démarrage, UID, cgroup), PID 1 et threads noyau, déduplication des
  disques, interfaces physiques, GPU sans valeurs inventées, 15 cycles de
  stabilité.
- `dotmon-app` : rendu headless des états « Indisponible » / « VEILLE » et
  mécanisme « fermeture désactivée » (aucun dialogue de confirmation possible ;
  la fermeture est maintenant **activée** par défaut, voir Phase 3).

## Phase 3 : fermeture réelle des processus

Le test `controller_is_disabled_for_real_processes` (phase 2) affirmait que le
contrôleur Linux refusait tout : ce comportement a **volontairement changé** ;
il est remplacé par les tests ci-dessous. Les 133 autres tests sont conservés.
Total : 186 tests (core 72, sys 65, app 49), `--release`, clippy `-D warnings`
et rustfmt propres.

Cibles d'intégration : **uniquement des `sleep` créés par le test** (enfants du
banc d'essai, tués et récupérés à la fin même en cas d'échec) ; aucun processus
de la session n'est visé. Vérifié après les tests : aucun `sleep` résiduel, RTX
toujours `suspended`.

| Exigence | Test(s) |
|---|---|
| SIGTERM réel, sortie observée | `sys::sigterm_ends_a_cooperative_process…` (le statut de sortie du fils est « signal 15 ») |
| SIGKILL réel | `sys::sigkill_ends_the_process` (signal 9) |
| Processus restant actif après SIGTERM, pas d'escalade | `sys::sigterm_ignored_leaves…`, `app::engine::process_ignoring_sigterm…`, `a_sigterm_command_only_ever_produces_one_sigterm`, `real::sigterm_then_explicit_sigkill…` |
| Absence de fermeture automatique | `engine::never_ends_without_a_command` |
| Confirmation annulée / à usage unique / signal non substituable | `presenter::cancel_discards_request`, `end_requires_confirmation…`, `confirmation_is_bound_to_its_signal…`, `every_request_gets_a_fresh_batch…` |
| PID réutilisé | `sys::stale_identity_is_refused…` (PID réel, heure de démarrage fausse : processus intact), `control::identity_mismatch_sends_nothing`, `presenter::reused_pid_with_a_new_identity_cancels…` |
| Processus disparu avant la confirmation / l'action | `presenter::vanished_selection…`, `sys::process_that_ended_before…`, `control::vanished_process…`, `control::zombie_is_already_ended` |
| Processus protégé | `core::actions::protected_processes_are_refused…`, `engine::protected_processes…`, `real::engine_refuses_dotmon_init…`, `sys::the_controller_refuses_init_itself…` |
| Processus d'un autre utilisateur | `core::actions::other_users…`, `engine::other_users…` (simulation : jamais signaler un vrai processus root) |
| Permissions insuffisantes | `control::kernel_permission_refusal_is_reported…` (faux noyau, EPERM) |
| Identité non vérifiable | `control::unverifiable_identity_is_refused` |
| Plusieurs actions simultanées | `sys::many_simultaneous_actions…` (8 + 1 + 1 cibles réelles, signaux mixtes), `engine::simultaneous_actions…` |
| Groupes | `core::actions::*` (principal seul / descendants / exclus protégés / contient Dotmon / script lanceur ambigu) |
| Messages | `outcomes::*`, scénario UI « 15-still-running » |
| Backend simulé | `mock::*`, `engine::*` sur la simulation |

Limites de la vérification : la chaîne complète « clic → processus réel tué »
n'a pas été pilotée par de vrais clics sous Hyprland (pas d'outil d'automatisation
de l'interface) ; elle est couverte par le test UI headless (callbacks réels)
d'une part, et par le moteur sur le backend Linux réel avec des processus
jetables d'autre part. Le protocole § 8 de `docs/PROTOCOLE.md` permet de la
vérifier à la main.
