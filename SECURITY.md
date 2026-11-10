# Sécurité

Dotmon envoie des signaux à des processus. Ses garde-fous (confirmation, identité
PID + heure de démarrage, `pidfd`, liste de processus protégés, refus des processus
d'autres utilisateurs, aucune escalade automatique) sont décrits dans
[`docs/PHASE3.md`](docs/PHASE3.md).

Dotmon ne s'exécute **jamais** en root et n'utilise ni `sudo` ni Polkit.

## Signaler une vulnérabilité

Merci de ne pas ouvrir de ticket public pour un contournement des protections
(par exemple un processus protégé qu'on peut fermer, ou une confirmation qui
atteint une autre cible). Utilisez le signalement privé de vulnérabilités de
GitHub (onglet *Security* → *Report a vulnerability*) du dépôt.

Versions concernées : la dernière version publiée.
