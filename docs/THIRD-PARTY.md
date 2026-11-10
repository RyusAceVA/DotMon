# Licences et composants tiers

Dotmon est distribué sous **GPL-3.0-only** (fichier `LICENSE`).

## Pourquoi la GPL-3.0

L'interface repose sur [Slint](https://slint.dev), proposé sous trois licences au
choix : **GPL-3.0-only**, licence *royalty-free* (avec obligations d'attribution)
ou licence commerciale. Dotmon utilise Slint sous **GPL-3.0** ; l'exécutable qui
l'incorpore est donc distribué sous GPL-3.0, et le code de Dotmon l'est aussi.
Les 442 autres dépendances sont sous des licences permissives (MIT, Apache-2.0,
BSD, Zlib, ISC, Unicode-3.0…) compatibles avec la GPL-3.0, plus une dépendance
MPL-2.0 (`option-ext`, via `dirs`), également compatible.

Le résumé exact se régénère avec `scripts/third-party.sh`.

## Polices embarquées dans le binaire

| Police | Usage | Licence | Texte |
|---|---|---|---|
| Doto | chiffres et titres à matrice de points | SIL OFL 1.1 | `crates/dotmon-app/ui/fonts/OFL-Doto.txt` |
| Space Mono | texte courant | SIL OFL 1.1 | `crates/dotmon-app/ui/fonts/OFL-SpaceMono.txt` |

Les paquets installent ces textes dans `/usr/share/licenses/dotmon/`.

## Bibliothèques système chargées à l'exécution

`fontconfig`, `wayland`, `libxkbcommon`, `libglvnd` (EGL) : chargées dynamiquement,
non redistribuées. `libnvidia-ml.so.1` (NVML, paquet `nvidia-utils`) : chargée
uniquement si présente et si le GPU NVIDIA est déjà éveillé.

## Marques et design

Dotmon s'inspire librement de l'esthétique « matrice de points » de Nothing OS.
Il ne reproduit aucun logo, aucune police ni aucun élément propriétaire de
Nothing. L'icône (`packaging/icons/dotmon.svg`) est une création originale. Dotmon
n'est ni affilié à, ni approuvé par Nothing Technology Limited.
