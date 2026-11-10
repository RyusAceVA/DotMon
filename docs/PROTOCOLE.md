# Protocole de validation sur machine réelle (Dotmon 0.2 → 0.3)

Ce protocole valide les **mesures** (lecture seule). Pour tester la
**fermeture réelle** sans risque pour la session, voir le § 8 en fin de document.

But : comparer ce que **Dotmon affiche** aux outils de CachyOS, repérer les
écarts de CPU, RAM, GPU, réseau et disques, puis ne corriger que les écarts
**confirmés** (définition au § 5).

Règles pendant toute la validation :

- aucune commande en root, aucune modification de la configuration de
  CachyOS ou de Hyprland ;
- la carte NVIDIA (par exemple une RTX) ne doit **jamais** être réveillée par la validation (sauf à
  l'étape G, facultative, qui la réveille volontairement et le dit) ;
- pendant ce protocole de mesures, ne fermez aucun processus avec Dotmon (la fermeture est testée au § 8, séparément).

Durée : environ 25 min sans l'étape G.

## Outils fournis

| Script | Rôle | Réveille la RTX ? |
|---|---|---|
| `scripts/compare.sh [N] [--nvidia-smi]` | N fenêtres d'une seconde : le backend de Dotmon et les outils système sont lus **sur les mêmes fenêtres**, avec un verdict par mesure (OK / ÉCART) et une synthèse. Le rapport est enregistré dans `target/validation/`. | Non. `--nvidia-smi` n'interroge la carte que si elle est **déjà** allumée. |
| `scripts/charge.sh …` | Charges contrôlées dont la valeur **attendue** est connue d'avance (CPU, RAM, disque, réseau) et affichée au lancement. | Non (sauf `charge.sh gpu`). |
| `scripts/gpu-veille.sh [DURÉE]` | Suit `runtime_status` et `runtime_active_time` de la RTX dans sysfs. Si `active_ms` ne bouge pas, la carte n'a pas été allumée, même 1 ms. | Non (sysfs uniquement). |

Références utilisées par `compare.sh` : `top` (CPU global et par processus),
`free`, `ps`, `df`, `sensors`, `/proc/loadavg`, `/proc/cpuinfo`, et les
compteurs sysfs `/sys/block/*/stat` et `/sys/class/net/*/statistics`
(ceux qu'affiche `ip -s link`). Ce sont d'autres interfaces que
`/proc/diskstats` et `/proc/net/dev`, que Dotmon lit lui-même.

### Outils à ne pas lancer pendant la validation (ils réveillent la RTX)

`nvidia-smi`, `nvtop`, `btop` si son module GPU NVIDIA est actif (il
charge NVML), `lspci -v` ou `-vv` (lecture de l'espace de configuration
PCI), `vulkaninfo`, `vkcube`, `prime-run`, et les applications lancées sur
le GPU dédié.

Sans risque, vérifié ici avec `gpu-veille.sh` (`active_ms` inchangé) :
`top`, `free`, `ps`, `df`, `sensors`, `ip`, `cat /sys/bus/pci/devices/*/power/*`
et `cat /proc/driver/nvidia/gpus/*/power`.

## 1. Préparation (2 min)

```fish
cd ~/Documents/Dev/TaskManager
cargo build --release
```

Ouvrez trois terminaux :

- **A** : `scripts/gpu-veille.sh 3600`, à laisser tourner du début à la fin ;
- **B** : `target/release/dotmon` (le pied de page doit indiquer `LINUX`) ;
- **C** : les commandes ci-dessous.

Vérifiez dans **A** que la RTX est `suspended`. Si elle est `active`,
fermez ce qui l'utilise (la colonne de droite donne une piste) et attendez
qu'elle passe à `suspended` avant de commencer.

## 2. Contrôle automatique au repos (3 min)

Ne touchez à rien pendant la mesure :

```fish
scripts/compare.sh 10
```

Lecture du rapport :

- `OK` : écart dans la tolérance ;
- `ÉCART  <<<` : hors tolérance dans cette fenêtre ;
- `info` : valeur donnée pour mémoire, sans tolérance ;
- **Synthèse** : `CONFIRMÉ` si l'écart revient dans au moins 60 % des
  fenêtres (≥ 6 sur 10), `ponctuel` sinon (en général deux lectures
  décalées de quelques millisecondes) ;
- dernière ligne : temps d'activité de la RTX pendant la mesure. Attendu :
  `suspended → suspended, 0 ms`.

Code de sortie : `0` si aucun écart n'est confirmé, `2` sinon.

Tolérances utilisées :

| Mesure | Référence | Tolérance | Pourquoi |
|---|---|---|---|
| CPU global | `top` (100 − id − wa) | ± 5 points | fenêtres décalées de quelques ms |
| CPU d'un processus | `top`, % d'un cœur | ± max(4 points, 25 %) | Dotmon × 16 = top |
| Biais CPU par processus | `top`, processus ≥ 50 % d'un cœur | ± 1,5 % | erreur systématique (corrigée, voir § 7) |
| RAM totale, swap total, taille d'un disque | `free`, `df` | exacte | aucune raison de différer |
| RAM utilisée / disponible / cache | `free -b` | ± 64 Mio | lectures successives |
| Swap utilisé | `free -b` | ± 16 Mio | idem |
| Processus / threads | `ps` | ± 6 / ± 30 | `ps` et `top` sont eux-mêmes des processus |
| Mémoire (RSS) de chaque processus | `ps` | ± max(4 Mio, 5 %), ≤ 2 % de processus hors tolérance | lectures successives |
| Espace utilisé | `df` (taille − disponible) | ± 64 Mio | écritures entre deux lectures |
| Débits disque | `/sys/block/*/stat` | ± max(20 %, 1 Mio/s) | fenêtres décalées |
| Débits réseau | `/sys/class/net/*/statistics` | ± max(15 %, 32 Kio/s) | idem |
| Température CPU (Tctl) | `sensors` | ± 3 °C | lecture instantanée |
| iGPU AMD : utilisation | `gpu_busy_percent` | ± 25 points | valeur instantanée très variable (indicatif) |
| iGPU AMD : VRAM, température | sysfs, `sensors` | ± 32 Mio, ± 3 °C | |
| RTX : état | `power/runtime_status` | cohérent | « suspended » doit donner VEILLE, sans aucune valeur |
| Fréquence moyenne | `/proc/cpuinfo` | ± 25 % | change en permanence (indicatif) |

## 3. Scénarios à valeur attendue (affichage de Dotmon)

`compare.sh` vérifie les mesures du backend. Cette étape vérifie
**l'écran** : chaque charge produit une valeur connue d'avance.
`charge.sh` l'affiche au lancement (« ATTENDU DANS DOTMON : … »).

Unités de Dotmon : Ko, Mo et Go sont en base 1024 (= Kio, Mio, Gio). Ce sont
les mêmes valeurs que `free -h` (Gi) et `df -h` (G).

Pour chaque scénario :

1. lancez la charge dans **C** ;
2. attendez 3 s que les valeurs se stabilisent, puis comparez l'écran à la
   valeur attendue et à l'outil indiqué ;
3. pendant que la charge tourne, lancez `scripts/compare.sh 5` dans un
   quatrième terminal ;
4. notez les écarts (fiche au § 6).

### A. Référence au repos

Tableau de bord (`Ctrl+1`) : relevez CPU, RAM utilisée, débits réseau et
disques. Ces valeurs servent de base aux scénarios suivants.

### B. CPU : `scripts/charge.sh cpu 4 60`

- **Attendu** : CPU global = base + 25 % (4 cœurs sur 16). Quatre barres
  de cœurs pleines (pas toujours les mêmes, l'ordonnanceur les déplace).
  Page Processus (`Ctrl+2`), vue PROCESSUS, recherche `yes` : quatre lignes
  à ≈ 6,2 % chacune. Pendant la première seconde d'un processus, la colonne
  affiche « — » (pas de mesure), puis la valeur.
- **Outil** : `top`, puis la touche `I` (majuscule), qui passe top en
  « mode Solaris » : les pourcentages par processus sont alors divisés par
  16 et directement comparables à Dotmon. Appuyez de nouveau sur `I` pour
  revenir au mode par défaut. Ce réglage n'est enregistré que si vous
  appuyez sur `W`.
- Vue APPS : les quatre `yes` sont regroupés sous le terminal qui les a
  lancés, et leur utilisation s'ajoute à sa ligne.

### C. RAM : `scripts/charge.sh ram 4 60`

- **Attendu** : MÉMOIRE utilisée = base + 4,0 Go, DISPONIBLE = base − 4,0 Go.
  Processus `python3` à ≈ 4,0 Go.
- **Outil** : `free -h` (lignes `utilisé` et `disponible`) et
  `ps -o pid,rss,comm -C python3` (RSS en Kio).

### D. Disques

```fish
scripts/charge.sh ecriture 50 30    # crée ~/.cache/dotmon-validation/test.bin (1,5 Gio)
scripts/charge.sh lecture 50 30
scripts/charge.sh nettoyer
```

- **Attendu** : E/S DISQUE, ÉCRITURE ≈ 50,0 Mo/s, puis LECTURE ≈ 50,0 Mo/s.
  L'autre sens reste proche de 0. Les accès sont directs (`O_DIRECT`) : ils
  passent par le disque, pas par le cache mémoire, et le débit est donc
  régulier.
- **Outil** : `compare.sh 5` pendant la charge (`/sys/block/nvme1n1/stat`).
- Une copie de fichier ordinaire, sans `O_DIRECT`, s'écrit d'abord en
  mémoire puis part sur le disque par rafales, jusqu'à 30 s plus tard. C'est
  le noyau qui fonctionne ainsi : tous les outils voient les mêmes rafales.
- STOCKAGE : « utilisé » dans Dotmon = `Taille − Disp.` de `df -h / /boot`,
  et non la colonne `Util.`, qui ne compte pas la réserve de btrfs
  (≈ 745 Mio sur `/`).

### E. Réseau : `scripts/charge.sh reseau URL 5 30`

Choisissez une URL de gros fichier à laquelle vous faites confiance, par
exemple une archive du noyau sur `cdn.kernel.org` ou une image ISO d'un
miroir CachyOS. Rien n'est écrit sur le disque.

- **Attendu** : RÉSEAU, réception ≈ 5,0 Mo/s si la connexion le permet,
  sinon le débit moyen affiché par `curl` à la fin. L'interface concernée
  est celle de la route par défaut (`ip route get 1.1.1.1`).
- **Outil** : `compare.sh 5` pendant la charge, ou `ip -s link show wlan0`
  deux fois à 10 s d'intervalle (différence des octets reçus / 10).
- Les totaux reçus et envoyés doivent correspondre à `ip -s link`, à
  l'octet près (en Mo arrondis à l'écran).

### F. Un processus précis

Choisissez une application dans Dotmon (par exemple firefox), vue
PROCESSUS, et notez son PID. Puis :

```fish
ps -o pid,user,rss,nlwp,etimes,args -p PID
```

| Dotmon (détail) | `ps` | Attendu |
|---|---|---|
| Mémoire | `RSS` (Kio) | identique à quelques Kio près (la valeur bouge) |
| Threads | `NLWP` | identique |
| Utilisateur | `USER` | identique |
| DÉMARRÉ « il y a … » | `ETIMES` (secondes) | identique à la seconde près |
| Ligne de commande | `ARGS` | identique (tronquée à l'écran) |
| E/S | — | « — » pour root et les autres utilisateurs (illisible sans privilèges) |

### G. GPU NVIDIA (facultatif, réveille la RTX volontairement)

À faire en dernier. Fermez toute fenêtre `nvidia-smi` avant de commencer :
un outil NVML ouvert empêche la carte de se rendormir.

```fish
scripts/charge.sh gpu 60
```

- Pendant la charge : la carte GPU de Dotmon affiche un pourcentage, la
  VRAM, la température et la puissance. La carte est déjà allumée, donc
  on peut comparer sans rien perturber :
  `scripts/compare.sh 5 --nvidia-smi` (tolérances : ± 15 points, ± 16 Mio,
  ± 2 °C, ± 5 W).
- Après l'arrêt (terminal **A**) : la carte doit repasser à `suspended`.
  Dès que Dotmon la mesure inactive, il affiche « REPOS » et cesse de
  l'interroger (30 s au plus). Il affiche « VEILLE » quand le pilote l'a
  éteinte, environ 20 s après la dernière interrogation lors des essais.
- **Écart confirmé** si la carte reste `active` plus de 2 min après l'arrêt
  alors que Dotmon tourne, et qu'elle s'éteint quand on ferme Dotmon.

### H. Veille et stabilité (10 min, sans rien faire)

Laissez Dotmon ouvert 10 min. Pendant ce temps, dans **C** :

```fish
ps -o pid,%cpu,rss,nlwp -C dotmon     # au début, puis à la fin
```

- **Attendu** : RSS stable (≈ 30 Mio, pas de croissance continue),
  CPU ≈ 3–4 % d'un cœur, 6 threads.
- Terminal **A** : `active_ms` inchangé du début à la fin, tant que rien
  d'autre n'utilise la RTX.

## 4. Où chercher quand un écart apparaît

```
Écart à l'écran ?
├─ compare.sh dit ÉCART sur la même mesure  → problème de mesure (backend dotmon-sys)
├─ compare.sh dit OK, l'écran diffère        → problème d'affichage (format, pont Slint)
└─ seulement pendant 1–2 s                   → décalage de fenêtres : non confirmé
```

Commande utile pour voir les mesures brutes du backend :
`cargo run --release -p dotmon-sys --example probe -- 5`.

## 5. Qu'est-ce qu'un écart confirmé ?

Un écart est confirmé, et peut alors être corrigé, s'il remplit les
quatre conditions :

1. il est **hors tolérance** (tableau du § 2) ;
2. il est **reproductible** : `CONFIRMÉ` dans la synthèse de `compare.sh`,
   ou observé à l'écran lors de trois essais séparés ;
3. un **second outil** donne la même référence que le premier (par
   exemple `free` et `/proc/meminfo`, ou `top` et `ps`) ;
4. il ne figure pas dans les **différences normales** (§ 7 et
   `docs/VALIDATION.md`).

Sinon, notez-le comme « non confirmé » et ne le corrigez pas.

## 6. Fiche d'écart (à copier)

```
Mesure            :
Scénario          : (A…H, charge lancée)
Valeur Dotmon     : (écran)          backend (compare.sh) :
Référence         : (outil, valeur)  second outil :
Tolérance         :
Fréquence         : (x fenêtres sur N / x essais sur 3)
RTX pendant le test : (active_ms avant → après)
Rapport           : target/validation/rapport-….txt
Verdict           : confirmé / non confirmé / différence normale
```

## 7. Écarts relevés pendant la préparation et corrigés (phase 2.1)

Résultats de `compare.sh` sur cette machine (10/11/2026) : RAM, swap,
processus, threads, RSS, espace disque, débits disque et réseau, température,
charge et iGPU AMD sont dans les tolérances, souvent à l'identique. Trois
écarts avaient été confirmés ; ils sont corrigés (détails et mesures dans
`docs/VALIDATION.md`, § « Phase 2.1 ») :

1. **CPU par processus surestimé de 2 à 3,6 %** : le calcul de sysinfo
   divisait par les compteurs de `/proc/stat`, qui avancent à 97,7–98,1 % du
   temps réel. Dotmon calcule maintenant le temps CPU du processus rapporté
   au temps réellement écoulé, comme `top`. Biais mesuré après correction :
   −0,2 à +0,5 %.
2. **Processus né pendant la fenêtre affiché à 0 %** : Dotmon affiche
   maintenant « — » jusqu'à la seconde mesure.
3. **Puissance de l'APU AMD** (CPU compris) : elle n'est plus jamais
   présentée comme la puissance du GPU. Elle est étiquetée « SOC ».

Différences normales, à ne pas corriger : voir `docs/VALIDATION.md`
(CPU par processus exprimé en % de la machine, réserve btrfs dans « utilisé »,
E/S des autres utilisateurs indisponibles, fréquence indicative).

## 8. Tester une fermeture réelle sans risquer la session (phase 3)

Principe : ne viser **que des processus jetables** que vous lancez vous-même.
Jamais une application de la session (Hyprland, Quickshell, PipeWire,
navigateur, terminal qui exécute Dotmon…). Dotmon refuse de toute façon les
processus protégés, mais ce protocole ne s'appuie pas sur ce refus.

1. Terminal A : `cargo run --release` (ou `target/release/dotmon`).
2. Terminal B (autre terminal que celui de Dotmon) :
   ```fish
   sleep 600 &                                   # cible « coopérative »
   sh -c "trap '' TERM; exec sleep 601" &        # cible qui ignore SIGTERM
   ```
3. Dans Dotmon : page PROCESSUS, vue PROCESSUS (pas APPLICATIONS), recherche
   `sleep`. Deux lignes `sleep` apparaissent (PID 600/601 dans la commande).
4. **SIGTERM, cible coopérative** : sélectionner `sleep 600` → FERMER →
   le dialogue annonce « Envoyer SIGTERM à « sleep » (PID n) » → FERMER.
   Attendu : « SIGTERM envoyé… », puis « … s'est terminé ». Vérifier dans le
   terminal B : `jobs` indique `Terminated`.
5. **SIGTERM ignoré** : sélectionner `sleep 601` → FERMER → confirmer.
   Attendu : « … est toujours actif 5 s après SIGTERM. Rien n'a été forcé ».
   Dans B : `pgrep -af 'sleep 601'` voit toujours le processus.
6. **SIGKILL, action séparée** : même ligne → FORCER (bouton rouge) → le dialogue
   annonce SIGKILL et la perte de données → FORCER L'ARRÊT. Attendu : « … s'est
   terminé ».
7. **Refus** (rien ne doit se passer) : sélectionner `Hyprland`, `qs`,
   `pipewire`, `systemd`, `dotmon` et un processus root : pas de dialogue,
   message « Processus protégé : … ».
8. **Déjà terminé** : lancer `sleep 602 &`, le sélectionner dans Dotmon, le tuer
   depuis B (`kill %n`), puis FERMER dans Dotmon : le dialogue se ferme avec
   « Le processus s'est terminé entre-temps » (ou « Processus déjà terminé »).
9. Nettoyage : `pkill -x sleep` (vos `sleep` seulement, pas de root).
10. Vérifier que la RTX est restée en veille :
    `cat /sys/bus/pci/devices/<adresse PCI du GPU>/power/runtime_status` → `suspended`
    (adresse : `lspci -D | grep -i nvidia`, par exemple `0000:01:00.0`).
