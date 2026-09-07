# Plan — `kepler top` ne compte pas la RAM des processus enfants

**Statut :** exécuté — voir [Résultats](#résultats)
**Date :** 2026-09-05 (plan), 2026-09-06 (exécution)
**Symptôme rapporté :** sur une VM Linux, `kepler top` sous-évalue la RAM d'un service qui a des processus enfants.

## Table des matières

- [Diagnostic](#diagnostic)
  - [Défaut A — enfants échappés du cgroup](#défaut-a--enfants-échappés-du-cgroup)
  - [Défaut B — parcours d'arbre aveugle dans le fallback](#défaut-b--parcours-darbre-aveugle-dans-le-fallback)
- [Phase 1 — Reproduire](#phase-1--reproduire)
- [Phase 2 — Corriger](#phase-2--corriger)
- [Phase 3 — Valider](#phase-3--valider)
- [Risques et points ouverts](#risques-et-points-ouverts)
- [Checklist d'exécution](#checklist-dexécution)
- [Résultats](#résultats)

---

## Diagnostic

Deux défauts indépendants produisent le même symptôme. Les deux doivent être traités : corriger l'un laisse l'autre actif selon la configuration de la machine.

| | Défaut A | Défaut B |
|---|---|---|
| **Chemin concerné** | cgroup v2 actif | fallback `ProcessGroup` |
| **Quand** | Linux, daemon root, `/sys/fs/cgroup` accessible en écriture | macOS/BSD **et** Linux si le daemon n'est pas root |
| **Cause** | enfants forkés avant `register_pid` → hors cgroup | `collect_descendants` parcourt une table `sysinfo` jamais peuplée |
| **Reproduction** | probabiliste (course), forçable | déterministe (100 %) |

### Défaut A — enfants échappés du cgroup

Ordre au spawn dans `kepler-daemon/src/process/mod.rs:128-150` :

1. `prepare_spawn()` — création du cgroup
2. `spawn_detached()` → `cmd.spawn()` — **le service tourne déjà**
3. récupération des pipes, spawn des tâches de capture, `.await`
4. `register_pid()` — écriture du PID dans `cgroup.procs`

Écrire un PID dans `cgroup.procs` migre **ce seul processus**, jamais ses descendants déjà existants (sémantique cgroup v2). Tout enfant forké entre 2 et 4 reste dans le cgroup du daemon, définitivement.

Le code connaît déjà ce phénomène — `kepler-daemon/src/containment.rs:128-130` :

> *"while killpg catches children that forked before `register_pid` moved the leader into the cgroup (they inherit the parent's original cgroup)"*

Le chemin de **kill** compense avec un `killpg` en plus du `cgroup.kill`. Le chemin de **monitoring** ne compense pas : `enumerate_service_pids()` (`containment.rs:71-79`) lit uniquement `cgroup.procs`, et le collector ne bascule sur le parcours d'arbre que si la liste est *vide* (`collector.rs:47`) — ce qui n'arrive jamais, le leader y étant toujours.

Services typiquement touchés : tout ce qui forke immédiatement après `exec` — wrappers `sh -c`, `npm start`, `python manage.py runserver` avec autoreload, superviseurs applicatifs.

### Défaut B — parcours d'arbre aveugle dans le fallback

`collector.rs:111-122` — `collect_descendants()` itère sur `sys.processes()`. Or `sys` n'est jamais rafraîchi autrement qu'avec `ProcessesToUpdate::Some(&pids_connus)` (`collector.rs:65-68`), et `Some(...)` n'insère jamais de nouveaux processus dans la table. Blocage circulaire : on ne rafraîchit que les PID déjà connus, donc on ne découvre jamais les enfants, donc on ne les rafraîchit jamais. Le résultat est toujours le seul PID principal.

Vérifié expérimentalement (réplique exacte de la logique du collector, sysinfo 0.33, `sh` + un enfant) : 1 PID vu au lieu de 2, 800 Ko comptés au lieu de 6400 Ko.

Ce défaut n'est **pas** limité à macOS : `detect_cgroupv2()` (`kepler-unix/src/cgroup/mod.rs:16-51`) doit créer `/sys/fs/cgroup/kepler`, ce qui exige root. Daemon lancé en utilisateur normal sur une VM Linux ⇒ stratégie `ProcessGroup` ⇒ défaut B.

> `docs/platform-compatibility.md:33` annonce « Process containment (cgroup v2) : Linux = Yes » sans mentionner la condition root. À corriger en phase 2.3.

---

## Phase 1 — Reproduire

**Objectif : deux tests rouges qui échouent pour la bonne raison, avant d'écrire la moindre ligne de correctif.**

### 1.0 — Outillage : un allocateur mémoire déterministe

L'image de test (`Dockerfile`, `rust:1.93-slim-bookworm`) n'a **pas** python3. Il faut un binaire qui alloue une quantité connue de RAM résidente et la garde.

Créer `kepler-e2e/src/bin/memhog.rs` :

```rust
//! Test helper: alloue N Mo de RAM résidente et les garde jusqu'à SIGTERM.
//! Utilisé par les tests de monitoring pour vérifier la comptabilisation
//! de la mémoire des processus enfants.
fn main() {
    let mb: usize = std::env::args().nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(200);
    let mut buf = vec![0u8; mb * 1024 * 1024];
    // Toucher chaque page pour forcer la résidence (sinon pas de RSS)
    for i in (0..buf.len()).step_by(4096) {
        buf[i] = 1;
    }
    std::hint::black_box(&buf);
    std::thread::sleep(std::time::Duration::from_secs(3600));
}
```

Déclarer la cible dans `kepler-e2e/Cargo.toml` (le `[lib]` explicite désactive l'auto-découverte) :

```toml
[[bin]]
name = "memhog"
path = "src/bin/memhog.rs"
```

`cargo build --workspace` le place dans `target/debug/memhog`, déjà sur le `PATH` du conteneur (`docker-compose.yml:15`). Aucune modification d'`entrypoint.sh` nécessaire.

**Choix de 200 Mo :** l'écart entre « parent seul » (~1-3 Mo) et « parent + enfant » (~200 Mo) est de deux ordres de grandeur. L'assertion est un seuil franc, pas une comparaison floue — pas de test instable.

### 1.1 — Reproduire le défaut B (déterministe)

Fixture `kepler-e2e/config/monitor_children_test/test_child_memory_counted.kepler.yaml` :

```yaml
kepler:
  monitor:
    interval: 1s

services:
  forker:
    # Le shell parent reste minuscule ; tout le RSS est dans l'enfant.
    command: ["sh", "-c", "memhog 200 & wait"]
```

Test `kepler-e2e/tests/monitor_children_test.rs` :

```rust
#[tokio::test]
async fn test_child_process_memory_is_counted() -> E2eResult<()> {
    // ... démarrage harness + services, attente de ~3 échantillons ...
    let json = /* kepler top --json */;
    let entry = &json["forker"];

    let rss = entry["memory_rss"].as_u64().unwrap();
    let pids = entry["pids"].as_array().unwrap();

    assert!(pids.len() >= 2,
        "le service a un enfant : au moins 2 PID attendus, vu {} ({:?})",
        pids.len(), pids);
    assert!(rss > 150 * 1024 * 1024,
        "l'enfant détient 200 Mo : RSS attendu > 150 Mo, vu {} Mo",
        rss / 1024 / 1024);
}
```

Exécution — **service compose non privilégié**, donc pas de cgroup, donc chemin fallback :

```bash
docker compose run --rm test cargo test -p kepler-e2e --test monitor_children_test -- --nocapture
```

**Attendu avant correctif :** échec sur `pids.len() >= 2` (1 PID vu), RSS ~1-3 Mo.

Confirmer au passage que la stratégie est bien celle qu'on croit, dans les logs du daemon :

```
Process containment: process groups (killpg fallback)
```

### 1.2 — Reproduire le défaut A (course, à forcer)

Même test, mais dans le service compose **privilégié**, où cgroup v2 est actif :

```bash
docker compose run --rm test-cgroup cargo test -p kepler-e2e --test monitor_children_test -- --nocapture
```

Log attendu : `Process containment: cgroup v2 (root: "/sys/fs/cgroup/kepler")`.

Ce test peut passer **par intermittence** : si le daemon gagne la course, l'enfant naît dans le cgroup et est compté. Une reproduction fiable exige d'élargir la fenêtre. Patch **temporaire, non commité**, dans `kepler-daemon/src/process/mod.rs` entre `spawn_detached()` et `register_pid()` :

```rust
// REPRO UNIQUEMENT — élargit la fenêtre de course pour rendre
// l'échappement du cgroup déterministe. À RETIRER.
tokio::time::sleep(std::time::Duration::from_millis(300)).await;
```

Avec ce patch, l'enfant est systématiquement forké avant l'écriture dans `cgroup.procs`. Vérification directe de l'échappement, en parallèle du test :

```bash
# Ce que voit le collector
cat /sys/fs/cgroup/kepler/*/forker/cgroup.procs
# La réalité
ps -eo pid,ppid,rss,comm --forest
```

**Attendu :** `cgroup.procs` contient 1 PID (le shell), `ps` en montre 2, et le PID de `memhog` se trouve dans le cgroup du daemon.

**Retirer le `sleep` avant de passer en phase 2.** Il ne sert qu'à établir le diagnostic.

### Critère de sortie de la phase 1

- [ ] `memhog` construit et disponible sur le `PATH` du conteneur
- [ ] Test rouge en mode `test` (défaut B), échec sur le nombre de PID
- [ ] Échappement du cgroup constaté directement (défaut A), `sleep` de repro retiré
- [ ] Les deux lignes de log `Process containment:` observées, une par mode

---

## Phase 2 — Corriger

### 2.1 — Collector : union cgroup + arbre de processus

`kepler-daemon/src/monitor/collector.rs`. Deux changements :

**a) Un `refresh` complet, une fois par cycle, hors de la boucle par service.**

`ProcessesToUpdate::All` est indispensable : sans lui le parcours d'arbre est aveugle (défaut B). Le sortir de la boucle par service corrige aussi un défaut mineur — actuellement le refresh est fait N fois par cycle pour N services.

**b) Union au lieu de fallback conditionnel.**

Les PID du cgroup et le parcours d'arbre depuis le PID principal sont deux sources **complémentaires**, pas une source et sa roue de secours :

- le cgroup rattrape les processus qui se sont détachés (re-parentés à init) — invisibles pour le parcours d'arbre ;
- le parcours d'arbre rattrape les enfants échappés du cgroup (défaut A).

Squelette :

```rust
let mut sys = System::new();
let config_hash = handle.config_hash().to_string();

loop {
    tokio::time::sleep(config.interval).await;
    let running = handle.get_running_services().await;
    if running.is_empty() { continue; }

    // Un seul refresh complet par cycle : le parcours d'arbre a besoin
    // d'une table de processus à jour, et les deltas CPU sont ainsi
    // calculés sur l'intervalle complet pour tous les services.
    sys.refresh_processes(ProcessesToUpdate::All, true);

    for service_name in &running {
        // Source 1 : le cgroup (attrape les processus détachés)
        let mut pids = containment.enumerate_service_pids(&config_hash, service_name);

        // Source 2 : l'arbre depuis le PID principal (attrape les enfants
        // forkés avant register_pid, qui sont restés hors du cgroup)
        if let Some(state) = handle.get_service_state(service_name).await
            && let Some(pid) = state.pid
        {
            if !pids.contains(&pid) { pids.push(pid); }
            collect_descendants(&sys, pid, &mut pids);
        }

        pids.sort_unstable();
        pids.dedup();   // impératif : un doublon double le RSS
        if pids.is_empty() { continue; }

        // ... somme cpu/rss/vss inchangée, plus aucun refresh ici ...
    }
}
```

Points de vigilance :

- **`dedup()` obligatoire.** Un PID présent dans les deux sources serait compté deux fois. C'est le principal risque de régression de ce correctif — un test doit le couvrir explicitement (phase 3).
- **`collect_descendants` doit être appelée après le refresh `All`**, sinon rien ne change.
- **Ne plus appeler `refresh_processes` dans la boucle par service.** Un second refresh rapproché écraserait le delta CPU et renverrait des valeurs proches de zéro.

### 2.2 — `kepler-exec` : supprimer la course à la source

Le correctif 2.1 *rattrape* les enfants échappés. Il vaut mieux qu'ils n'échappent pas : le cgroup sert aussi au containment (kill, limites), pas seulement à la mesure.

Principe : faire rejoindre le cgroup **avant l'`exec`**, quand le processus n'a encore aucun enfant. `kepler-exec` est le point de passage naturel.

- **Daemon** — `kepler-daemon/src/process/spawn.rs:167-217` (`build_command`) : ajouter un champ `cgroup_path` à `CommandSpec`, l'inclure dans la condition `needs_wrapper` (ligne 173), et passer `--cgroup <path>` à `kepler-exec`.
- **`kepler-exec`** — `kepler-exec/src/main.rs` : nouvel argument `--cgroup`, traité **entre la ligne 77 (`apply_rlimits`) et la ligne 87 (`drop_privileges`)**. L'ordre est critique : l'écriture dans `cgroup.procs` exige les droits root, donc elle doit précéder le `setuid`. En cas d'échec, journaliser sur stderr et continuer — ne jamais empêcher le service de démarrer pour un problème de cgroup.
- `register_pid()` dans le daemon devient une ceinture-bretelles : à conserver, il couvre le cas où le wrapper n'est pas utilisé.

Compromis à acter avant de coder :

| Approche | Avantage | Coût |
|---|---|---|
| `--cgroup` dans `kepler-exec` (**recommandé**) | supprime la course ; réutilise un binaire déjà dans le chemin | force le wrapper (un `exec` de plus) pour tout service sous cgroup, même sans `user`/`limits` |
| `pre_exec` dans le daemon | pas de binaire intermédiaire | fait perdre `posix_spawnp()` — explicitement rejeté par le projet (`kepler-exec/src/main.rs:4-5`) |
| Ne rien faire, s'appuyer sur 2.1 | zéro risque | la course reste ; le containment garde son trou |

**2.2 est séparable de 2.1.** Si le surcoût du wrapper généralisé pose problème, livrer 2.1 seul corrige le symptôme utilisateur ; 2.2 se traite dans un second temps.

### 2.3 — Documentation

- `docs/platform-compatibility.md:33` — préciser que cgroup v2 sur Linux exige un daemon root avec `/sys/fs/cgroup` accessible en écriture, et qu'à défaut c'est le fallback `ProcessGroup`.
- `docs/architecture.md` — décrire la double source de PID du collector et pourquoi c'est une union.

---

## Phase 3 — Valider

### 3.1 — Les tests de repro passent au vert

Exactement les mêmes commandes qu'en phase 1, sans aucune modification des tests. C'est la seule preuve qui compte : **un test écrit après le correctif ne prouve rien.**

```bash
docker compose run --rm test        cargo test -p kepler-e2e --test monitor_children_test -- --nocapture
docker compose run --rm test-cgroup cargo test -p kepler-e2e --test monitor_children_test -- --nocapture
```

Les deux doivent passer. Le test devient déterministe des deux côtés après correctif : que l'enfant soit dans le cgroup ou non, l'union le trouve.

### 3.2 — Tests complémentaires à ajouter

| Test | Vérifie | Contre quelle régression |
|---|---|---|
| `test_no_double_counting` | service **sans** enfant : `pids.len() == 1`, RSS cohérent avec `memhog` seul | le `dedup()` de 2.1 |
| `test_grandchild_memory_counted` | `sh -c "sh -c 'memhog 200 & wait' & wait"` | la récursion de `collect_descendants` |
| `test_detached_child_counted` (cgroup uniquement) | enfant re-parenté à init, invisible pour l'arbre | que l'union n'ait pas été remplacée par le seul parcours d'arbre |
| `test_cgroup_contains_child_pid` (cgroup uniquement) | après 2.2, l'enfant est **dans** `cgroup.procs` | la fermeture de la course elle-même |

Le dernier test est celui qui distingue « on rattrape le problème » (2.1) de « le problème n'existe plus » (2.2).

### 3.3 — Non-régression

```bash
docker compose run --rm test        cargo test --workspace
docker compose run --rm test-cgroup cargo test --workspace
```

Surveiller en particulier `kepler-e2e/tests/monitor_top_test.rs` (les assertions existantes sur `pids` et `memory_rss` changent de valeur, pas de forme) et `kepler-tests/tests/cgroup_tests.rs` (impacté par 2.2).

### 3.4 — Coût du `refresh` complet

`ProcessesToUpdate::All` scanne tout `/proc` à chaque intervalle, contre quelques PID ciblés auparavant. C'est ce que fait `htop`, mais il faut le mesurer plutôt que le supposer — un daemon qui monitore à `interval: 1s` sur une machine chargée en paiera le prix.

Protocole : machine avec plusieurs centaines de processus, `interval: 1s`, mesurer le CPU du daemon avant/après sur 5 minutes. Si le surcoût est notable, l'atténuation évidente est de ne faire le refresh `All` que lorsqu'au moins un service est en stratégie fallback ou a des enfants connus — mais ne pas optimiser avant d'avoir le chiffre.

### 3.5 — Validation sur la VM d'origine

Les conteneurs ne remplacent pas la machine où le bug a été constaté. Sur la VM Linux, avec un vrai service qui forke :

```bash
kepler top --json | jq '.<service> | {memory_rss, pids}'
ps -eo pid,ppid,rss,comm --forest   # comparer la somme des RSS
```

La somme des RSS de l'arbre doit correspondre à `memory_rss`, à quelques pourcents près.

---

## Risques et points ouverts

- **Double comptage des pages partagées — ✅ traité séparément (voir plus bas).** Sommer le RSS de plusieurs processus compte deux fois les pages partagées par `fork()`/COW. Le collector lit désormais `memory.current` du cgroup quand il est disponible. Attention : ça ne corrige **ni** le défaut A **ni** le défaut B — les enfants échappés du cgroup restent invisibles pour `memory.current` comme pour la somme des RSS, et le chemin fallback n'est pas concerné du tout. Les phases 1 à 3 ci-dessus restent entièrement à faire.
- **La course du défaut A est probabiliste.** Sa reproduction dépend de la charge machine. D'où le `sleep` temporaire en 1.2 — et d'où l'intérêt de 2.2, qui l'élimine plutôt que de la rattraper.
- **2.2 généralise le wrapper `kepler-exec`** à tous les services sous cgroup, y compris ceux sans `user`/`limits`. Un `exec` supplémentaire par démarrage de service. À arbitrer.
- **Docker requis en local.** Aucun démon Docker ne tourne actuellement sur cette machine (`colima` est installé mais arrêté) ; il faut le démarrer avant la phase 1. `test-cgroup` exige `privileged: true` — vérifier que la VM `colima` expose bien cgroup v2 en écriture, sinon la partie 1.2 devra se faire directement sur la VM Linux.

## Checklist d'exécution

1. [ ] Démarrer Docker (`colima start`), vérifier que `docker compose run --rm test-cgroup` voit cgroup v2
2. [ ] Écrire `memhog` + la fixture + le test `monitor_children_test`
3. [ ] **Constater l'échec** en mode `test` (défaut B)
4. [ ] **Constater l'échappement du cgroup** en mode `test-cgroup` avec le `sleep` temporaire (défaut A), puis retirer le `sleep`
5. [ ] Appliquer 2.1 (collector : refresh `All` + union + `dedup`)
6. [ ] Rejouer 2 et 3 → vert des deux côtés
7. [ ] Arbitrer 2.2, l'appliquer le cas échéant, ajouter `test_cgroup_contains_child_pid`
8. [ ] Tests complémentaires 3.2
9. [ ] Non-régression `--workspace` dans les deux modes
10. [ ] Mesurer le coût du refresh `All` (3.4)
11. [ ] Mettre à jour la documentation (2.3)
12. [ ] Valider sur la VM Linux d'origine (3.5)

---

## Résultats

### Ce que la reproduction a montré

Les deux défauts se sont reproduits, tous deux en rouge, avec le même symptôme : **1 PID rapporté au lieu de 2 (et de 3)**.

Deux écarts par rapport au plan :

1. **Le défaut A n'a pas eu besoin d'être forcé.** Le `sleep` temporaire prévu en 1.2 s'est révélé inutile : l'enfant gagne la course systématiquement. Un service qui forke immédiatement après `exec` bat le daemon à tous les coups, ce qui rend le défaut bien moins « probabiliste » que je ne le supposais en écrivant le plan.
2. **Le premier jet de `memhog` faussait la mesure.** Il touchait ses pages une fois puis dormait — des pages anonymes froides, que le noyau récupère sous pression, d'où des RSS à 117 Mo au lieu de 200. Il les retouche désormais en boucle, et les tests attendent par polling au lieu d'un `sleep` fixe.

### Ce que le correctif 2.1 seul n'a pas suffi à corriger

Après l'union, les PID étaient corrects dans les deux modes — mais sur le chemin cgroup la mémoire tombait à **0 Mo**.

Cause : avant l'union, `memory.current` et la somme des RSS voyaient exactement le même ensemble de processus (tous deux partaient de `cgroup.procs`), donc préférer le premier était sans risque. Avec l'union, la somme des RSS couvre aussi les enfants échappés alors que `memory.current` ne compte que les membres du cgroup — préférer `memory.current` faisait donc *disparaître* l'enfant. Ce risque avait été écarté dans l'analyse initiale ; il est devenu réel au moment précis où l'union est arrivée.

D'où deux conséquences :

- **2.2 est passé d'optionnel à obligatoire.** Rattraper l'échappement côté monitoring ne suffit pas quand la source d'accounting, elle, ne rattrape rien.
- **Un garde-fou a été ajouté** : `memory.current` n'est utilisé que si le cgroup contient effectivement tous les PID énumérés. Entre sur-évaluer des pages partagées et perdre un processus entier, la somme des RSS est le moindre mal.

### Fichiers touchés

| Fichier | Changement |
|---|---|
| `kepler-daemon/src/monitor/collector.rs` | refresh `All` une fois par cycle ; union cgroup + arbre ; `dedup` ; `visited` séparé de `result` dans `collect_descendants` ; garde-fou sur la source mémoire |
| `kepler-exec/src/main.rs` | `--cgroup` : rejoint le cgroup avant l'`exec` et avant le `setuid` |
| `kepler-daemon/src/process/command.rs` | champ `cgroup_path` sur `CommandSpec` |
| `kepler-daemon/src/process/spawn.rs` | `--cgroup` passé au wrapper ; `needs_wrapper` inclut `cgroup_path` |
| `kepler-daemon/src/process/mod.rs` | renseigne `spec.cgroup_path` avant le spawn |
| `kepler-daemon/src/containment.rs` | `service_cgroup_path` |
| `kepler-e2e/src/bin/memhog.rs` | charge de test |
| `kepler-e2e/tests/monitor_children_test.rs` | 5 tests |
| `kepler-e2e/config/monitor_children_test/` | 4 fixtures |
| `docs/architecture.md`, `docs/platform-compatibility.md` | sections « PID Enumeration » / « Memory Accounting » ; condition root sur cgroup v2 |

### Non-régression (§3.3)

`cargo test --workspace` complet, dans les deux environnements, sur le même arbre :

| Mode | Commande | Résultat |
|---|---|---|
| cgroup v2 | `docker compose run --rm test-cgroup` | 84 suites, **1758 passés / 0 échec**, 6 ignorés, exit 0 |
| repli | `docker compose run --rm test` | 84 suites, **1758 passés / 0 échec**, 6 ignorés, exit 0 |

Les 5 tests de `monitor_children_test` passent dans les deux modes. En mode repli, `test_forked_child_is_inside_the_cgroup` et `test_detached_child_memory_is_counted` sortent immédiatement via `require_cgroupv2()` — c'est le comportement voulu, ils n'ont de sens que là où un cgroup existe.

### Reste à faire

- **Le coût du refresh `All` (§3.4) n'a pas été mesuré.** Le scan complet de `/proc` à chaque intervalle est ce que fait `htop`, mais sur une machine chargée avec `interval: 1s` ça mérite un chiffre avant de considérer le sujet clos.
- **La validation sur la VM d'origine (§3.5)** reste à faire par toi : les conteneurs ne remplacent pas la machine où le bug a été constaté.

---

## Annexe — comptabilité mémoire via `memory.current` (fait)

Livré hors du périmètre des défauts A et B, qui restent ouverts.

**Problème.** Sommer le RSS par processus double-compte les pages partagées en copy-on-write après un `fork()`. Mesuré dans le conteneur privilégié, sur un parent de 60 Mo qui forke 2 enfants sans `exec` : **somme des RSS = 173 Mo, `memory.current` = 58 Mo**, pour 60 Mo réellement alloués. C'est le modèle pre-fork (gunicorn, unicorn, php-fpm, workers nginx) : le service paraît N+1 fois plus gros qu'il n'est.

**Correctif.** `memory_rss` provient maintenant de `memory.current − inactive_file` du cgroup du service quand le contrôleur `memory` est disponible, sinon de la somme des RSS comme avant. La soustraction d'`inactive_file` est la formule « working set » de Docker et Kubernetes : sans elle, un service qui lit de gros fichiers paraîtrait les détenir en mémoire.

**Délégation du contrôleur.** `memory.current` n'existe dans un cgroup que si son parent liste `memory` dans `cgroup.subtree_control`. Kepler l'active sur ses propres niveaux (`/sys/fs/cgroup/kepler/` et `.../<config_hash>/`) mais **ne touche jamais au cgroup racine du système** — sur un hôte systemd la délégation est déjà faite, et ailleurs on retombe silencieusement sur la somme des RSS.

**Impact sur l'environnement de test.** Le conteneur privilégié n'a pas cette délégation : sa racine de namespace cgroup contient ses propres processus, ce qui interdit d'activer un contrôleur pour ses enfants (règle « no internal processes »). `entrypoint.sh` déplace donc ces processus dans un sous-cgroup `init` quand `REQUIRE_CGROUPV2=1`, reproduisant ce que systemd fait sur un vrai hôte. Conséquence : la racine du namespace n'accepte plus de processus, et `test_cgroupv2_required_when_env_set` a dû cesser de l'utiliser comme point de parking.

**Fichiers touchés.**

| Fichier | Changement |
|---|---|
| `kepler-unix/src/cgroup/mod.rs` | `enable_memory_controller`, `read_memory_current`, `read_memory_stat_field` ; activation dans `detect_cgroupv2` et `create_service_cgroup` |
| `kepler-daemon/src/containment.rs` | `ContainmentManager::service_memory_current` |
| `kepler-daemon/src/monitor/collector.rs` | `memory_rss` depuis le cgroup, repli sur la somme des RSS |
| `kepler-unix/src/cgroup/tests.rs` | `test_memory_current_readable`, `test_memory_current_does_not_double_count_cow`, helper `move_pid_out_of_service_cgroup` |
| `entrypoint.sh` | délégation `memory` dans le conteneur privilégié |
| `docs/architecture.md` | section « Memory Accounting » |

## Voir aussi

- [Testing](../testing.md) — harnesses, environnement Docker
- [Platform Compatibility](../platform-compatibility.md) — matrice cgroup v2
- [Privilege Dropping](../privilege-dropping.md) — rôle de `kepler-exec`
