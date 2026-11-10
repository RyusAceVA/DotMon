# Paquet Arch / CachyOS

Dotmon s'installe comme une application native : `/usr/bin/dotmon`, entrée de
lanceur et icônes aux emplacements Freedesktop. Rien n'est écrit dans le dossier
personnel à l'installation ; Dotmon ne crée que son fichier de réglages
`~/.config/dotmon/` quand vous modifiez un réglage.

## Contenu du paquet (`pacman -Ql dotmon`)

| Fichier | Rôle |
|---|---|
| `/usr/bin/dotmon` | binaire release (polices Doto et Space Mono, UI et logique embarquées) |
| `/usr/share/applications/dotmon.desktop` | entrée du lanceur (`Exec=dotmon`, `Icon=dotmon`) |
| `/usr/share/icons/hicolor/{16…512}x…/apps/dotmon.png`, `scalable/apps/dotmon.svg` | icônes |
| `/usr/share/licenses/dotmon/OFL-*.txt` | licences des polices embarquées |
| `/usr/share/doc/dotmon/*.md` | documentation |

Aucun service systemd, aucun hook, aucun fichier de configuration système.

## Dépendances

Vérifiées sur le binaire (`readelf -d`, bibliothèques chargées par `dlopen`) :

- **liées** : `glibc`, `gcc-libs`, `fontconfig` ;
- **chargées à l'exécution** par Slint/winit (Wayland natif) : `wayland`
  (`libwayland-client`), `libxkbcommon` (constaté avec `LD_DEBUG=libs` dans une session réelle) ;
- `hicolor-icon-theme` (hiérarchie du thème d'icônes, signalé par `namcap`) ;
- **`ttf-font`** (au moins une police système) : sans aucune police, la bibliothèque de
  texte (`fontique`) plante au démarrage (`NoMatch`) ; constaté sur un Arch minimal ;
- **optionnelle** : `libglvnd` (`libEGL`), seulement pour le rendu OpenGL facultatif
  (`SLINT_BACKEND=winit-femtovg`). Le rendu par défaut est logiciel et ne touche pas au GPU ;
- **optionnelle** : `nvidia-utils` (`libnvidia-ml.so.1`, NVML), chargée seulement
  si la RTX est déjà éveillée (la veille n'est jamais perturbée) ;
- **construction** : `cargo` (paquet `rust`).

## Construire et installer

En utilisateur normal, jamais avec sudo :

```fish
cd ~/Documents/Dev/TaskManager/packaging/arch
./dist.sh          # archive déterministe des sources + empreinte dans le PKGBUILD
makepkg -si        # compile en release, lance les tests, installe (pacman demande le mot de passe)
```

`makepkg --nocheck` saute les tests (ils ont déjà tourné). `cargo fetch` a besoin
du réseau la première fois (puis le cache `~/.cargo` suffit).

## Mettre à jour

Après une modification des sources :

```fish
packaging/arch/update.sh
```

Le script relance `dist.sh` puis `makepkg -fsi`. Pour une nouvelle version,
changez la version dans `Cargo.toml` (`[workspace.package]`) **et** `pkgver`
dans `packaging/arch/PKGBUILD` (`dist.sh` refuse les versions différentes) ; pour
une simple reconstruction de la même version, augmentez `pkgrel`.

## Tester dans un Arch propre

Les scripts de la CI se lancent à l'identique dans un conteneur ou chroot Arch jetable :

```bash
# en root, dans l'Arch propre (conteneur archlinux:base-devel, devtools, chroot…)
scripts/ci/install-deps.sh          # dépendances lues dans packaging/arch/PKGBUILD
# puis en utilisateur normal (makepkg refuse root) :
scripts/ci/check.sh                 # fmt, clippy -D warnings, tests
scripts/ci/package.sh               # shellcheck, desktop-file-validate, makepkg, namcap, chemins personnels
```

Cela a été validé sur un rootfs Arch officiel (image *bootstrap*, sans aucun paquet de
bureau ni police) exécuté sans root dans un espace de noms utilisateur : les 186 tests
passent, le paquet se construit, `namcap` n'émet que des avertissements attendus
(bibliothèques chargées par `dlopen`), et le binaire installé démarre sous un compositeur
Wayland headless (`weston --backend=headless`).

## Désinstaller

```fish
sudo pacman -Rns dotmon
```

Retire tous les fichiers du paquet. Vos réglages éventuels (`~/.config/dotmon/`)
restent ; supprimez-les à la main si vous le souhaitez.

## Vérifier l'installation

```fish
pacman -Qi dotmon ; pacman -Ql dotmon ; pacman -Qkk dotmon     # fichiers et intégrité
desktop-file-validate /usr/share/applications/dotmon.desktop
gtk-launch dotmon                                              # lancement via l'entrée desktop
```

## Dépannage : l'icône de Dotmon manque dans le lanceur `Super` (Quickshell `ii`)

**Cause (vérifiée).** Le lanceur d'`ii` résout les icônes par nom avec
`Quickshell.iconPath()`, c'est-à-dire avec le chargeur d'icônes de Qt, qui
mémorise la liste des icônes du thème **pendant toute la vie du processus**. Un
Quickshell lancé *avant* l'installation de Dotmon ne voit donc pas
`/usr/share/icons/hicolor/*/apps/dotmon.png`, même si le fichier existe, si le cache
GTK est à jour (`gtk-update-icon-cache` est exécuté par pacman) et si `gtk-launch`
trouve l'icône. Un Quickshell démarré après l'installation résout `Icon=dotmon`
normalement. Ce n'est pas propre à Dotmon : toute nouvelle application a le même
effet jusqu'au redémarrage de Quickshell. Un `Icon=/chemin/absolu.png` contourne le
chargeur (c'est ce qui avait fonctionné), mais n'est pas nécessaire et ne suit pas le
thème d'icônes.

**Solution sans redémarrer Quickshell** (dans votre session, en utilisateur normal) :

```fish
dbus-send --session --type=signal /KIconLoader org.kde.KIconLoader.iconChanged int32:0
```

`ii` écoute ce signal KDE standard et invalide son cache d'icônes. Un redémarrage de
Quickshell ou une reconnexion produit le même résultat. Le paquet affiche cette
commande après l'installation (`dotmon.install`).

**Aucun fichier personnel n'est nécessaire.** Si vous aviez créé
`~/.local/share/applications/dotmon.desktop` pour contourner le problème, vous pouvez
le supprimer : il masque l'entrée du paquet (même identifiant) et ne suit plus ses mises à jour.

## Dépannage général du lanceur

Le lanceur lit les fichiers `.desktop` via `DesktopEntries` de Quickshell
(dossiers de `$XDG_DATA_DIRS`, donc `/usr/share/applications`) et surveille
leurs changements.

1. `ls /usr/share/applications/dotmon.desktop` : absent → paquet non installé (`pacman -Q dotmon`).
2. `desktop-file-validate /usr/share/applications/dotmon.desktop` : doit être silencieux.
3. `gtk-launch dotmon` : doit ouvrir la fenêtre. Sinon, lancez `dotmon` dans un terminal
   et lisez l'erreur (bibliothèque manquante, etc.).
4. Entrée absente : redémarrer Quickshell (`qs kill -c ii` puis `qs -c ii`, ou le
   raccourci de rechargement d'`ii`).
5. Recherchez « Dotmon », « tâches » ou « moniteur » (mots-clés de l'entrée).
6. Fenêtre sans icône dans la barre : la classe de fenêtre est `dotmon`
   (`hyprctl clients | grep class`), identique à `StartupWMClass`.
