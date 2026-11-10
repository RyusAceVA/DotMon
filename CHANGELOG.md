# Journal des modifications

Format inspiré de [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) ;
versions selon [SemVer](https://semver.org/lang/fr/).

## [0.4.0-beta.1] — 2026-11-10

Première version bêta publique.

### Ajouté
- Intégration bureau : entrée `.desktop`, icône originale (SVG + PNG, thème `hicolor`).
- Paquets Arch : `packaging/arch` (construction locale), modèles AUR `dotmon` et `dotmon-bin`.
- Intégration continue (tests, clippy, paquet) et workflow de release.
- Licence GPL-3.0-only, documentation des composants tiers.

### Corrigé pendant la préparation
- Dépendances de paquet vérifiées sur Arch propre : ajout de `hicolor-icon-theme` et `ttf-font`
  (le binaire plantait sans aucune police système), `libglvnd` devient optionnelle.

### Déjà présent dans cette version
- Métriques réelles : CPU, mémoire, GPU (NVIDIA via NVML, AMD via sysfs), disques, réseau.
- Veille de la carte NVIDIA préservée (aucune interrogation d'un GPU endormi).
- Liste de processus et regroupement en applications.
- Fermeture de processus : SIGTERM (« FERMER ») et SIGKILL (« FORCER ») distincts,
  confirmation liée à l'identité exacte (PID + heure de démarrage), `pidfd`,
  protections (PID 1, threads noyau, Dotmon, session Hyprland/Quickshell, PipeWire,
  portails, services système, autres utilisateurs), jamais d'escalade automatique.
- Mode `--mock` (machine simulée, démonstration et tests).

### Limitations connues
- Interface uniquement en français.
- Testé sur CachyOS / Hyprland (Wayland) ; pas de support X11.
- Voir `docs/PHASE3.md` pour les limites de la fermeture d'applications multi-processus.
