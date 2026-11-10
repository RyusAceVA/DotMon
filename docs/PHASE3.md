# Phase 3 : fermeture réelle des processus

Dotmon peut maintenant terminer de vrais processus Linux, **sans root, sans
sudo, sans Polkit**, uniquement après une demande explicite et une confirmation.

## Deux actions distinctes

| Action | Signal | Bouton | Confirmation |
|---|---|---|---|
| **FERMER** | `SIGTERM` | gris/blanc | « Envoyer SIGTERM à « nom » (PID n) … S'il ne se ferme pas, rien ne sera forcé. » |
| **FORCER** | `SIGKILL` | rouge, bordure rouge du dialogue | « … arrêt immédiat. Les données non enregistrées seront perdues. » |

- **Jamais de SIGKILL automatique** après un SIGTERM, ni de second envoi. Si le
  processus ignore SIGTERM, Dotmon l'écrit (« toujours actif 5 s après
  SIGTERM. Rien n'a été forcé ») ; l'utilisateur choisit alors FORCER, ce qui
  rouvre un dialogue avec sa propre confirmation.
- Une confirmation est **liée à un signal et à des identités exactes**
  (PID + heure de démarrage). Le signal est porté par la demande : une
  confirmation SIGTERM ne peut pas produire SIGKILL. Elle est à usage unique
  (`confirm()` consomme la demande) et chaque demande a un numéro de lot.

## Comment un processus est visé (`dotmon-sys/src/linux/control.rs`)

Un PID seul n'est pas une identité (il peut être réattribué). Dotmon utilise
`pidfd_open(2)` / `pidfd_send_signal(2)` (via `rustix` 1.1) :

1. `pidfd_open(pid)` : le descripteur désigne le processus qui possède ce PID
   *maintenant* et continue de le désigner même si le numéro est recyclé ;
2. lecture de `/proc/<pid>/stat` **après** l'ouverture du pidfd : l'heure de
   démarrage doit être celle confirmée, sinon `PidReused` (rien n'est envoyé) ;
3. `pidfd_send_signal` sur ce descripteur : un seul processus, jamais de groupe,
   jamais de récursion ;
4. le pidfd est conservé pour **observer la sortie** du même processus.

Tout ce qui n'est pas vérifiable est refusé : `/proc` illisible (`hidepid`),
`stat` incompréhensible, noyau sans pidfd. `EPERM` du noyau est affiché tel quel
(« Accès refusé par le système ») : pas de contournement.

## Protections (empilées, chacune suffisante)

1. **Présentateur** (`core::actions::plan_end` + `ProtectionPolicy`) : pas de
   dialogue pour un processus protégé ; la raison est affichée.
2. **Moteur** : revérifie la cible contre son dernier échantillon et la
   politique avant d'appeler le contrôleur.
3. **Contrôleur Linux** : refuse PID 0/1/2, Dotmon lui-même, threads noyau
   (drapeau `PF_KTHREAD`, PPID 2), zombies ; vérifie l'identité avec le noyau.

Protégés : PID 1, threads noyau, Dotmon, Hyprland, Quickshell (`qs`), PipeWire
(`pipewire*`, `wireplumber`), portails (`xdg-desktop-portal*`), `systemd*`,
`dbus-*`, `polkitd`, `NetworkManager`, etc. (liste de `core::protection`),
et tout processus d'**un autre utilisateur** (affichés, jamais signalés).

## Applications (groupes) : règles et limitations

- Un processus choisi dans la vue PROCESSUS est visé **seul** : jamais ses
  enfants.
- **FERMER une application** = SIGTERM au **processus principal seul**.
  L'application ferme elle-même ses processus secondaires, comme à la
  fermeture de sa fenêtre.
- **FORCER une application** = SIGKILL au processus principal **et** aux
  descendants vérifiés au moment de la demande (liste explicite d'identités
  montrée par le nombre de processus). Rien n'est résolu récursivement plus
  tard : un processus apparu après la confirmation n'est pas touché. Chaque
  cible est revérifiée individuellement.
- Les membres protégés d'une application sont **exclus et comptés** (« N
  processus protégé(s) ne seront pas touchés ») ; si c'est le processus
  principal qui est protégé, la demande est refusée.
- **Refusés** (message explicite, utiliser la vue PROCESSUS) :
  - groupe contenant **Dotmon lui-même** (ex. le terminal qui l'a lancé) ;
  - groupe **ambigu** : racine = script lanceur (`bash`, `sh`…) ayant démarré
    zéro ou plusieurs programmes — on ne devine pas lequel est « l'application » ;
  - groupe de threads noyau.
- Un script lanceur avec **un seul** programme : la cible est le programme, pas
  le script.
- **Limitation** : l'appartenance d'un processus à une application repose sur
  la filiation (PPID) observée ; un processus réattribué à `init` après la mort
  de son parent n'est plus rattaché à son ancienne application. Si l'application
  utilise des processus détachés (daemons), FORCER ne les atteint pas.

## Résultat réel affiché

Un signal envoyé n'est pas un processus terminé. Trois étapes, chacune dite
honnêtement (toast ; avertissements affichés 8 s) :

1. « SIGTERM envoyé à « mpv » (PID 425) — en attente de sa fermeture » ;
2. « « mpv » (PID 425) s'est terminé » **ou** « … est toujours actif 5 s après
   SIGTERM. Rien n'a été forcé : utilisez FORCER si nécessaire » ;
3. refus précis : « Accès refusé par le système », « Processus déjà terminé »,
   « Identité du processus modifiée (PID réattribué) : rien n'a été envoyé »,
   « Processus protégé : … », « Identité non vérifiable (…) ».

Fenêtres d'observation : 5 s après SIGTERM, 3 s après SIGKILL. La sortie est
détectée par le pidfd (lisible dès la fin du processus). Pour une action sur
une application, les résultats sont regroupés en un seul message.

## Backend simulé (`--mock`)

Mêmes règles, sans toucher au système. `obsidian` ignore SIGTERM pour voir le
cas « toujours actif ». SIGKILL n'emporte que la cible (les descendants listés
sont des cibles explicites), contrairement à l'ancienne simulation.

## Tests

Voir `docs/VALIDATION.md`, § « Phase 3 ». Les tests d'intégration ne ciblent
que des processus `sleep` créés par le banc d'essai (enfants du test, tués et
récupérés à la fin même en cas d'échec).
