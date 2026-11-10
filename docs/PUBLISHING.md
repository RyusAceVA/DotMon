# Publier Dotmon (GitHub puis AUR)

Dépôt : <https://github.com/RyusAceVA/dotmon> · Mainteneur : `RyusAceVA`.
Rien de ce qui suit n'est exécuté automatiquement : chaque étape est lancée à la main.

## 0. Avant de commencer

- [ ] `scripts/ci/check.sh` (version, fmt, clippy, tests) et `scripts/ci/package.sh` passent en local.
- [ ] Le commit local `release: prepare Dotmon v0.4.0-beta.1` et l'étiquette annotée `v0.4.0-beta.1`
      existent (`git log --oneline`, `git tag -n`), et `git status` est propre.

## 1. Dépôt GitHub

Le dépôt `RyusAceVA/DotMon` a déjà été créé (vide, public) : GitHub ignore la casse dans les
URL, `RyusAceVA/dotmon` y mène. On y pousse donc directement, **sans** `gh repo create` :

```fish
cd ~/Documents/Dev/TaskManager
git remote add origin https://github.com/RyusAceVA/dotmon.git      # ou git@github.com:RyusAceVA/dotmon.git
git push -u origin main                                            # la branche seule, SANS l'étiquette
```

Attendre que l'onglet *Actions* affiche la CI (`CI`) verte. Si elle échoue, corriger avant
d'étiqueter. Facultatif : *Settings → General* pour renommer le dépôt en minuscules `dotmon`,
et *Settings → Code security* pour activer les signalements privés de vulnérabilité.

## 2. Release bêta

```fish
git push origin v0.4.0-beta.1
```

Cette poussée déclenche le workflow `Release` (seul déclencheur : une étiquette `v*`). Il :
vérifie que l'étiquette = version (Cargo, PKGBUILD, journal, notes `docs/releases/`), lance les
tests sur Arch, construit `dotmon-0.4.0-beta.1-x86_64-linux.tar.gz` (binaire) et
`dotmon-0.4.0-beta.1.tar.gz` (`git archive` de l'étiquette) avec leurs `.sha256`, puis crée la
release **pré-version** « Dotmon v0.4.0-beta.1 » avec les notes de `docs/releases/v0.4.0-beta.1.md`
et les sommes SHA-256.

Contrôler ensuite la release sur GitHub (pré-version cochée, 4 fichiers joints) et télécharger
une archive pour recalculer sa somme.

## 3. AUR

Prérequis : compte sur <https://aur.archlinux.org>, clé SSH enregistrée dans le profil.
Les SHA-256 ne peuvent être calculés qu'**après** la release (les archives doivent être publiées) :

```fish
cd ~/Documents/Dev/TaskManager
packaging/aur/finalize.sh
# → packaging/aur/out/{dotmon,dotmon-bin}/ : PKGBUILD, dotmon.install, .SRCINFO
#   (les sommes sont calculées sur les fichiers réellement téléchargés ET comparées aux .sha256 publiés)

for p in dotmon dotmon-bin
    git clone ssh://aur@aur.archlinux.org/$p.git /tmp/aur-$p      # dépôt vide : avertissement normal
    cp packaging/aur/out/$p/{PKGBUILD,dotmon.install,.SRCINFO} /tmp/aur-$p/
    cd /tmp/aur-$p
    makepkg --printsrcinfo | diff - .SRCINFO                      # doit être identique
    git add PKGBUILD dotmon.install .SRCINFO
    git commit -m "Initial import : dotmon 0.4.0_beta.1"
    git push                                                      # branche master
    cd -
end
```

Vérifier ensuite la construction depuis l'AUR dans un environnement propre
(`paru -S dotmon-bin`, ou un chroot Arch).

## 4. Publier une nouvelle version

1. Changer la version dans `Cargo.toml` (`[workspace.package]`) et `pkgver` dans
   `packaging/arch/PKGBUILD` et dans les deux `PKGBUILD.in` (`0.4.0-beta.2` ↔ `0.4.0_beta.2`) ;
   `CHANGELOG.md`, `docs/releases/vX.Y.Z.md` ; `cargo update --workspace`.
2. `scripts/ci/check.sh && scripts/ci/package.sh` (le premier vérifie la cohérence des versions).
3. Commit, étiquette annotée `vX.Y.Z`, push de la branche puis de l'étiquette, puis
   `finalize.sh` et push AUR (`pkgrel=1` pour une nouvelle version, +1 si seul le PKGBUILD change).

## Rappels

- `dotmon` (sources) et `dotmon-bin` (binaire) sont en conflit réciproque ; `dotmon-bin`
  déclare `provides=dotmon`.
- L'archive source de `dotmon` est celle jointe à la release (pas l'archive auto-générée
  par GitHub) : son contenu et sa somme ne changent plus une fois publiés.
- Ne jamais pousser `packaging/arch/*.pkg.tar.zst`, `src/`, `pkg/`, `target/` (déjà dans `.gitignore`).
- Les mainteneurs AUR doivent répondre aux commentaires et marquer le paquet obsolète
  quand une nouvelle version sort.
