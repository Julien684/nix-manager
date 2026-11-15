//! Travail en arrière-plan : un thread par opération, un canal de messages
//! consommé par la pompe `QTimer` du thread UI (aucun freeze de l'interface).

use std::process::Command;
use std::sync::mpsc::Sender;

use crate::nix::{self, Hit, NixState, ProfileEntry};

/// Opération en cours ou terminée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    State,
    Search,
    Profile,
    InstallNix,
    UninstallNix,
    InstallPkg,
    RemovePkg,
    Upgrade,
    Gc,
    Desktop,
    Sandbox,
    Subvol,
    Unfree,
    Zoneinfo,
}

/// Messages worker → interface.
pub enum Msg {
    /// Ligne de journal (sortie brute des commandes, en direct).
    Log(String),
    /// État de Nix relu.
    State(NixState),
    /// Résultats de recherche.
    Search(Vec<Hit>),
    /// Contenu du profil utilisateur.
    Profile(Vec<ProfileEntry>),
    /// Fin d'une opération.
    Done {
        kind: JobKind,
        ok: bool,
        detail: String,
    },
}

fn log(tx: &Sender<Msg>, line: impl AsRef<str>) {
    let _ = tx.send(Msg::Log(line.as_ref().to_string()));
}

fn done(tx: &Sender<Msg>, kind: JobKind, ok: bool, detail: impl Into<String>) {
    let _ = tx.send(Msg::Done {
        kind,
        ok,
        detail: detail.into(),
    });
}

// ------------------------------------------------------------------
// Lecture de l'état / du profil / de la recherche
// ------------------------------------------------------------------

pub fn spawn_state(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let state = nix::detect();
        let detail = match &state.version {
            Some(v) => format!("nix {v}"),
            None => "absent".to_string(),
        };
        let _ = tx.send(Msg::State(state));
        done(&tx, JobKind::State, true, detail);
    });
}

pub fn spawn_profile(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let Some(bin) = nix::nix_bin() else {
            done(&tx, JobKind::Profile, false, "nix absent".to_string());
            return;
        };
        let mut cmd = nix::nix_command(&bin, &["profile", "list", "--json"]);
        let mut logf = |line: String| log(&tx, line);
        match nix::capture_streaming(&mut cmd, &mut logf) {
            Ok(json) => {
                let entries = nix::parse_profile_json(&json);
                let count = entries.len();
                let _ = tx.send(Msg::Profile(entries));
                done(&tx, JobKind::Profile, true, format!("{count}"));
            }
            Err(e) => done(&tx, JobKind::Profile, false, format!("nix profile list : {e}")),
        }
    });
}

pub fn spawn_search(tx: Sender<Msg>, term: String) {
    std::thread::spawn(move || {
        let Some(bin) = nix::nix_bin() else {
            done(&tx, JobKind::Search, false, "nix absent".to_string());
            return;
        };
        log(&tx, format!("$ nix search nixpkgs {term} --json"));
        let mut cmd = nix::nix_command(&bin, &["search", "nixpkgs", &term, "--json"]);
        let mut logf = |line: String| log(&tx, line);
        match nix::capture_streaming(&mut cmd, &mut logf) {
            Ok(json) => {
                let hits = nix::parse_search_json(&json, 200, &term);
                let count = hits.len();
                let _ = tx.send(Msg::Search(hits));
                done(&tx, JobKind::Search, true, format!("{count}"));
            }
            Err(e) => done(&tx, JobKind::Search, false, format!("nix search : {e}")),
        }
    });
}

// ------------------------------------------------------------------
// Installation / désinstallation de Nix (root via pkexec)
// ------------------------------------------------------------------

/// Lance `pkexec <prog> <args…>` en diffusant la sortie.
fn pkexec_stream(tx: &Sender<Msg>, prog: &str, args: &[&str]) -> bool {
    if !nix::which("pkexec") {
        log(tx, "pkexec introuvable : impossible de demander les droits administrateur.");
        return false;
    }
    log(tx, format!("$ pkexec {prog} {}", args.join(" ")));
    let mut cmd = Command::new("pkexec");
    cmd.arg(prog).args(args);
    let mut logf = |line: String| log(tx, line);
    nix::run_streaming(&mut cmd, &mut logf)
}

/// Écrit un script temporaire exécutable en root.
fn write_script(name: &str, body: &str) -> Option<String> {
    let path = format!("/tmp/nix-manager-{name}-{}.sh", std::process::id());
    std::fs::write(&path, body).ok()?;
    Some(path)
}

pub fn spawn_install_nix(tx: Sender<Msg>, clean_first: bool) {
    std::thread::spawn(move || {
        // Un binaire présent sans démon = installation à moitié faite : elle
        // doit être nettoyée puis refaite, pas considérée comme « installée ».
        let state = nix::detect();
        if state.daemon && state.version.is_some() {
            done(
                &tx,
                JobKind::InstallNix,
                false,
                "Nix est déjà installé et le démon tourne".to_string(),
            );
            return;
        }
        if clean_first {
            log(&tx, "Nettoyage des restes d'une installation précédente…");
            let script = nix::manual_uninstall_script();
            match write_script("cleanup", &script) {
                Some(path) => {
                    let ok = pkexec_stream(&tx, "sh", &[path.as_str()]);
                    let _ = std::fs::remove_file(&path);
                    if !ok {
                        done(
                            &tx,
                            JobKind::InstallNix,
                            false,
                            "le nettoyage des restes a échoué (voir le journal)".to_string(),
                        );
                        return;
                    }
                }
                None => {
                    done(
                        &tx,
                        JobKind::InstallNix,
                        false,
                        "impossible d'écrire le script de nettoyage".to_string(),
                    );
                    return;
                }
            }
        }
        log(&tx, format!("Téléchargement de {}", nix::INSTALLER_URL));
        let script = format!("/tmp/nix-manager-install-{}.sh", std::process::id());
        let downloaded: Result<(), String> = ureq::get(nix::INSTALLER_URL)
            .timeout(std::time::Duration::from_secs(120))
            .call()
            .map_err(|e| e.to_string())
            .and_then(|resp| {
                let mut file = std::fs::File::create(&script).map_err(|e| e.to_string())?;
                std::io::copy(&mut resp.into_reader(), &mut file)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            });
        if let Err(e) = downloaded {
            done(
                &tx,
                JobKind::InstallNix,
                false,
                format!("téléchargement : {e}"),
            );
            return;
        }
        log(&tx, format!("Installeur enregistré : {script}"));
        log(&tx, "Installation en mode démon multi-utilisateur (mot de passe administrateur demandé)…");
        // On retient la dernière ligne utile pour expliquer un échec au lieu de
        // renvoyer l'utilisateur au journal.
        let mut last = String::new();
        let ok = {
            let mut logf = |line: String| {
                if !line.trim().is_empty() {
                    last = line.clone();
                }
                log(&tx, line);
            };
            if !nix::which("pkexec") {
                log(&tx, "pkexec introuvable : impossible de demander les droits administrateur.");
                false
            } else {
                log(&tx, format!("$ pkexec sh {script} --daemon --yes"));
                let mut cmd = Command::new("pkexec");
                cmd.arg("sh").args([script.as_str(), "--daemon", "--yes"]);
                nix::run_streaming(&mut cmd, &mut logf)
            }
        };
        let _ = std::fs::remove_file(&script);
        if !ok {
            let hint = if last.contains("backup-before-nix") || last.contains("already exists") {
                "restes d'une installation précédente : relancer puis accepter le nettoyage"
            } else if last.contains("nixbld") {
                "le groupe nixbld existe déjà : nettoyer les restes d'abord"
            } else if last.is_empty() {
                "l'installeur a échoué (voir le journal)"
            } else {
                "l'installeur a échoué (voir le journal)"
            };
            let detail = if last.is_empty() {
                hint.to_string()
            } else {
                let short: String = last.chars().take(160).collect();
                format!("{hint} — {short}")
            };
            done(&tx, JobKind::InstallNix, false, detail);
            return;
        }
        // Vérification finale : démon actif et binaire qui répond.
        let state = nix::detect();
        let ok = state.daemon && state.version.is_some();
        let detail = if ok {
            format!(
                "nix {} — nix-daemon actif",
                state.version.clone().unwrap_or_default()
            )
        } else if state.version.is_some() {
            "nix installé mais nix-daemon inactif".to_string()
        } else {
            "nix introuvable après installation".to_string()
        };
        done(&tx, JobKind::InstallNix, ok, detail);
    });
}

pub fn spawn_uninstall_nix(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let ok = if std::path::Path::new("/nix/nix-installer").exists() {
            log(&tx, "Désinstalleur officiel trouvé : /nix/nix-installer");
            pkexec_stream(&tx, "/nix/nix-installer", &["uninstall", "--no-confirm"])
        } else {
            log(&tx, "Pas de désinstalleur officiel : procédure manuelle documentée.");
            let script = nix::manual_uninstall_script();
            match write_script("uninstall", &script) {
                Some(path) => {
                    let ok = pkexec_stream(&tx, "sh", &[path.as_str()]);
                    let _ = std::fs::remove_file(&path);
                    ok
                }
                None => {
                    log(&tx, "Impossible d'écrire le script de désinstallation.");
                    false
                }
            }
        };
        if !ok {
            done(
                &tx,
                JobKind::UninstallNix,
                false,
                "la désinstallation a échoué (voir le journal)".to_string(),
            );
            return;
        }
        // Nettoyage des restes propres à l'utilisateur (sans root).
        cleanup_user_leftovers(&tx);
        let state = nix::detect();
        let cleared = !state.store && state.version.is_none();
        let detail = if cleared {
            "Nix supprimé, /nix absent".to_string()
        } else {
            "des restes subsistent (voir le journal)".to_string()
        };
        done(&tx, JobKind::UninstallNix, cleared, detail);
    });
}

/// Supprime les fichiers Nix du compte courant.
fn cleanup_user_leftovers(tx: &Sender<Msg>) {
    let home = nix::home();
    let mut removed = 0;
    for rel in [
        ".nix-profile",
        ".nix-defexpr",
        ".nix-channels",
        ".cache/nix",
        ".local/state/nix",
    ] {
        let path = home.join(rel);
        if path.symlink_metadata().is_ok() {
            let ok = if path.is_dir() && !path.is_symlink() {
                std::fs::remove_dir_all(&path).is_ok()
            } else {
                std::fs::remove_file(&path).is_ok()
            };
            if ok {
                removed += 1;
                log(tx, format!("supprimé : {}", path.display()));
            }
        }
    }
    let conf = home.join(".config/environment.d/60-nix-xdg.conf");
    if std::fs::remove_file(&conf).is_ok() {
        removed += 1;
        log(tx, format!("supprimé : {}", conf.display()));
    }
    // Liens .desktop pointant dans le profil Nix ou le magasin (y compris
    // devenus orphelins après la suppression de /nix : la cible lue est
    // ~/.nix-profile/…, pas /nix/store/…).
    let apps = home.join(".local/share/applications");
    if let Ok(rd) = std::fs::read_dir(&apps) {
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(target) = std::fs::read_link(&path) else {
                continue;
            };
            let target = target.to_string_lossy();
            if target.contains("/nix/") || target.contains(".nix-profile") {
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                    log(tx, format!("supprimé : {}", path.display()));
                }
            }
        }
    }
    log(tx, format!("{removed} reste(s) utilisateur supprimé(s)."));
}

// ------------------------------------------------------------------
// Profil utilisateur (sans root)
// ------------------------------------------------------------------

fn run_nix(tx: &Sender<Msg>, kind: JobKind, args: &[&str]) -> bool {
    let Some(bin) = nix::nix_bin() else {
        done(tx, kind, false, "nix absent".to_string());
        return false;
    };
    log(tx, format!("$ nix {}", args.join(" ")));
    let mut cmd = nix::nix_command(&bin, args);
    let mut logf = |line: String| log(tx, line);
    nix::run_streaming(&mut cmd, &mut logf)
}

/// Commande `nix` prête à lancer, avec l'autorisation des paquets non libres
/// quand elle est activée (variable d'environnement + `--impure`, comme le
/// recommande Nix pour les flakes).
fn nix_cmd(bin: &str, args: &[&str], unfree: bool) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(nix::FEATURES);
    if unfree {
        cmd.env("NIXPKGS_ALLOW_UNFREE", "1");
    }
    cmd.args(args);
    // `--impure` est une option d'évaluation PAR SOUS-COMMANDE : placée avant
    // elle, nix répond « unrecognised flag '--impure' ». Elle doit donc suivre
    // les arguments de la sous-commande.
    if unfree {
        cmd.arg("--impure");
    }
    cmd
}

pub fn spawn_pkg_install(tx: Sender<Msg>, targets: Vec<String>) {
    std::thread::spawn(move || {
        let Some(bin) = nix::nix_bin() else {
            done(&tx, JobKind::InstallPkg, false, "nix absent".to_string());
            return;
        };
        let unfree = nix::unfree_enabled();
        let mut installed: Vec<String> = Vec::new();
        for target in &targets {
            log(
                &tx,
                format!(
                    "$ {}nix profile install {target}{}",
                    if unfree { "NIXPKGS_ALLOW_UNFREE=1 " } else { "" },
                    if unfree { " --impure" } else { "" }
                ),
            );
            let args = ["profile", "install", target.as_str()];
            let mut cmd = nix_cmd(&bin, &args, unfree);
            let mut logf = |line: String| log(&tx, line);
            if !nix::run_streaming(&mut cmd, &mut logf) {
                let detail = if !unfree {
                    format!("échec : {target} (paquet non libre ? voir Maintenance)")
                } else {
                    format!("échec : {target}")
                };
                done(&tx, JobKind::InstallPkg, false, detail);
                return;
            }
            installed.push(target.clone());
        }
        done(&tx, JobKind::InstallPkg, true, installed.join(", "));
    });
}

pub fn spawn_pkg_remove(tx: Sender<Msg>, names: Vec<String>) {
    std::thread::spawn(move || {
        let Some(bin) = nix::nix_bin() else {
            done(&tx, JobKind::RemovePkg, false, "nix absent".to_string());
            return;
        };
        let mut removed: Vec<String> = Vec::new();
        for name in &names {
            log(&tx, format!("$ nix profile remove {name}"));
            let args = ["profile", "remove", name.as_str()];
            let mut cmd = nix_cmd(&bin, &args, false);
            let mut logf = |line: String| log(&tx, line);
            if !nix::run_streaming(&mut cmd, &mut logf) {
                done(&tx, JobKind::RemovePkg, false, format!("échec : {name}"));
                return;
            }
            removed.push(name.clone());
        }
        done(&tx, JobKind::RemovePkg, true, removed.join(", "));
    });
}

pub fn spawn_upgrade(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let ok = run_nix(&tx, JobKind::Upgrade, &["profile", "upgrade", "--all"]);
        done(
            &tx,
            JobKind::Upgrade,
            ok,
            if ok { "profil à jour".to_string() } else { "échec".to_string() },
        );
    });
}

pub fn spawn_gc(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let tool = nix::sibling_tool("nix-collect-garbage");
        log(&tx, format!("$ {tool} -d"));
        let mut cmd = Command::new(&tool);
        cmd.arg("-d");
        let mut logf = |line: String| log(&tx, line);
        let ok = nix::run_streaming(&mut cmd, &mut logf);
        done(
            &tx,
            JobKind::Gc,
            ok,
            if ok { "magasin nettoyé".to_string() } else { "échec".to_string() },
        );
    });
}

// ------------------------------------------------------------------
// Intégration bureau
// ------------------------------------------------------------------

pub fn spawn_desktop(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let home = nix::home();
        let apps_dir = home.join(".local/share/applications");
        let mut written = 0;
        let mut failed = 0;
        if let Err(e) = std::fs::create_dir_all(&apps_dir) {
            done(&tx, JobKind::Desktop, false, format!("{} : {e}", apps_dir.display()));
            return;
        }
        let sources = nix::desktop_entries();
        // Nettoyage : nos entrées générées dont le paquet n'est plus installé.
        if let Ok(rd) = std::fs::read_dir(&apps_dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if !content.contains(nix::DESKTOP_MARKER) {
                    continue;
                }
                let still_there = sources
                    .iter()
                    .any(|s| s.file_name() == path.file_name());
                if !still_there {
                    if std::fs::remove_file(&path).is_ok() {
                        log(&tx, format!("périmé, supprimé : {}", path.display()));
                    }
                }
            }
        }
        for src in &sources {
            let Some(name) = src.file_name() else { continue };
            let dst = apps_dir.join(name);
            let text = match std::fs::read_to_string(src) {
                Ok(t) => t,
                Err(e) => {
                    failed += 1;
                    log(&tx, format!("lecture {} : {e}", src.display()));
                    continue;
                }
            };
            // Copie réécrite, pas un lien : le menu doit trouver le binaire et
            // l'icône sans dépendre du PATH ni de XDG_DATA_DIRS de la session.
            let entry = nix::parse_desktop_entry(&text);
            let label = entry
                .name
                .clone()
                .unwrap_or_else(|| name.to_string_lossy().into_owned());
            let (content, changed) = nix::rewrite_desktop_file(&text);
            // Remplace un éventuel lien symbolique laissé par une version
            // précédente de l'outil.
            let _ = std::fs::remove_file(&dst);
            match std::fs::write(&dst, content) {
                Ok(()) => {
                    written += 1;
                    if changed {
                        log(
                            &tx,
                            format!("réécrit : {label} (Exec/Icon en chemins absolus)"),
                        );
                    } else {
                        log(&tx, format!("copié : {label}"));
                    }
                }
                Err(e) => {
                    failed += 1;
                    log(&tx, format!("écriture {} : {e}", dst.display()));
                }
            }
        }
        // Icônes et thèmes : les sessions ne lisent pas ~/.nix-profile/share.
        let conf_dir = home.join(".config/environment.d");
        let conf = conf_dir.join("60-nix-xdg.conf");
        let body = format!(
            "# Généré par Nix Manager\n\
XDG_DATA_DIRS={home}/.nix-profile/share:/usr/local/share:/usr/share\n\
PATH={home}/.nix-profile/bin:/nix/var/nix/profiles/default/bin:$PATH\n",
            home = home.display()
        );
        let conf_ok = std::fs::create_dir_all(&conf_dir)
            .and_then(|()| std::fs::write(&conf, body))
            .is_ok();
        if conf_ok {
            log(&tx, format!("écrit : {}", conf.display()));
        } else {
            log(&tx, format!("échec de l'écriture de {}", conf.display()));
        }
        let ok = conf_ok && failed == 0;
        done(
            &tx,
            JobKind::Desktop,
            ok,
            format!(
                "{written} entrée(s) de menu, environment.d : {}",
                if conf_ok { "écrit" } else { "échec" }
            ),
        );
    });
}

pub fn spawn_sandbox(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let script = nix::sandbox_script();
        let Some(path) = write_script("sandbox", &script) else {
            done(
                &tx,
                JobKind::Sandbox,
                false,
                "impossible d'écrire le script".to_string(),
            );
            return;
        };
        let ok = pkexec_stream(&tx, "sh", &[path.as_str()]);
        let _ = std::fs::remove_file(&path);
        done(
            &tx,
            JobKind::Sandbox,
            ok,
            if ok { "sandbox = false".to_string() } else { "échec".to_string() },
        );
    });
}

/// Crée ou supprime le sous-volume btrfs `/nix` (root via pkexec).
///
/// Créer un sous-volume dédié sort le magasin des instantanés de `/` — c'est
/// la recommandation par défaut, à faire AVANT la première installation.
pub fn spawn_subvol(tx: Sender<Msg>, create: bool) {
    std::thread::spawn(move || {
        if !nix::which("btrfs") {
            done(
                &tx,
                JobKind::Subvol,
                false,
                "btrfs-progs absent (paquet btrfs-progs ?)".to_string(),
            );
            return;
        }
        let ran = if create {
            match nix::nix_path_state() {
                nix::NixPath::Absent => {
                    log(&tx, "création du sous-volume btrfs /nix…");
                    pkexec_stream(&tx, "btrfs", &["subvolume", "create", "/nix"])
                }
                nix::NixPath::EmptyDir => {
                    // Un répertoire vide fait échouer `subvolume create`
                    // (« File exists ») : le script le supprime d'abord.
                    log(&tx, "/nix est un répertoire vide : suppression puis création…");
                    let script = nix::subvol_create_script();
                    match write_script("subvol", &script) {
                        Some(path) => {
                            let ok = pkexec_stream(&tx, "sh", &[path.as_str()]);
                            let _ = std::fs::remove_file(&path);
                            ok
                        }
                        None => {
                            log(&tx, "impossible d'écrire le script de création.");
                            false
                        }
                    }
                }
                nix::NixPath::Subvolume => {
                    done(
                        &tx,
                        JobKind::Subvol,
                        false,
                        "/nix est déjà un sous-volume btrfs".to_string(),
                    );
                    return;
                }
                nix::NixPath::Mounted | nix::NixPath::NonEmptyDir => {
                    done(
                        &tx,
                        JobKind::Subvol,
                        false,
                        "/nix existe déjà et contient des données".to_string(),
                    );
                    return;
                }
            }
        } else {
            log(&tx, "suppression du sous-volume btrfs /nix…");
            // -R : supprime aussi le contenu (btrfs-progs ≥ 6.7).
            pkexec_stream(&tx, "btrfs", &["subvolume", "delete", "-R", "/nix"])
        };
        // Vérification par l'état réel, pas par le code de retour du script.
        let now_subvol = nix::is_subvolume("/nix");
        let (ok, detail) = if create {
            (
                ran && now_subvol,
                if now_subvol {
                    "/nix est maintenant un sous-volume btrfs".to_string()
                } else {
                    "création du sous-volume impossible (voir le journal)".to_string()
                },
            )
        } else {
            let gone = !std::path::Path::new("/nix").exists();
            (
                ran && gone,
                if gone {
                    "sous-volume /nix supprimé".to_string()
                } else {
                    "suppression du sous-volume impossible (voir le journal)".to_string()
                },
            )
        };
        done(&tx, JobKind::Subvol, ok, detail);
    });
}

/// Active/désactive l'autorisation des paquets non libres dans le fichier
/// `~/.config/nixpkgs/config.nix`, sans toucher aux autres réglages.
pub fn apply_unfree(path: &std::path::Path, enable: bool) -> std::io::Result<()> {
    if enable {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, nix::UNFREE_CONFIG)?;
        return Ok(());
    }
    if !path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(path)?;
    // On retire uniquement l'affectation `allowUnfree = … ;`, pas la ligne
    // entière : une config sur une seule ligne garde ses autres réglages.
    let mut out_lines: Vec<String> = Vec::new();
    for line in content.lines() {
        if !line.replace(' ', "").contains("allowUnfree") {
            out_lines.push(line.to_string());
            continue;
        }
        let mut s = line.to_string();
        while let Some(pos) = s.find("allowUnfree") {
            let end = s[pos..].find(';').map(|e| pos + e + 1).unwrap_or(s.len());
            s.replace_range(pos..end, "");
        }
        if !s.trim().is_empty() {
            out_lines.push(s);
        }
    }
    // Fichier créé par nous (ou ne contenant plus que les accolades) : on le
    // supprime plutôt que de laisser un « { } » inutile.
    let meaningful = out_lines.iter().any(|l| {
        l.chars()
            .any(|c| !c.is_whitespace() && c != '{' && c != '}')
    });
    if meaningful {
        std::fs::write(path, out_lines.join("\n"))?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub fn spawn_unfree(tx: Sender<Msg>, enable: bool) {
    std::thread::spawn(move || {
        let path = nix::nixpkgs_config_path();
        match apply_unfree(&path, enable) {
            Ok(()) => {
                log(&tx, format!("écrit : {}", path.display()));
                let now = nix::unfree_enabled();
                done(
                    &tx,
                    JobKind::Unfree,
                    now == enable,
                    if enable {
                        "paquets non libres autorisés".to_string()
                    } else {
                        "paquets non libres interdits".to_string()
                    },
                );
            }
            Err(e) => done(&tx, JobKind::Unfree, false, format!("{}: {e}", path.display())),
        }
    });
}

pub fn spawn_zoneinfo(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let script = nix::zoneinfo_script();
        let Some(path) = write_script("zoneinfo", &script) else {
            done(
                &tx,
                JobKind::Zoneinfo,
                false,
                "impossible d'écrire le script".to_string(),
            );
            return;
        };
        let ran = pkexec_stream(&tx, "sh", &[path.as_str()]);
        let _ = std::fs::remove_file(&path);
        let now = nix::zoneinfo_ok();
        done(
            &tx,
            JobKind::Zoneinfo,
            ran && now,
            if now {
                "/etc/zoneinfo → /usr/share/zoneinfo".to_string()
            } else {
                "création du lien impossible (voir le journal)".to_string()
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfree_active_et_retire_sans_toucher_au_reste() {
        let dir = std::env::temp_dir().join(format!("nixmgr-unfree-{}", std::process::id()));
        let path = dir.join("nixpkgs/config.nix");
        let _ = std::fs::remove_dir_all(&dir);

        apply_unfree(&path, true).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("allowUnfree = true"));

        apply_unfree(&path, false).unwrap();
        assert!(!path.exists(), "le fichier créé par nous doit disparaître");

        // Un fichier utilisateur avec d'autres réglages est conservé.
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ allowUnfree = true; permittedInsecurePackages = [ ]; }\n").unwrap();
        apply_unfree(&path, false).unwrap();
        let left = std::fs::read_to_string(&path).unwrap();
        assert!(!left.contains("allowUnfree"));
        assert!(left.contains("permittedInsecurePackages"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn messages_envoyes_dans_l_ordre() {
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        log(&tx, "a");
        done(&tx, JobKind::Gc, true, "ok");
        match rx.try_recv() {
            Ok(Msg::Log(l)) => assert_eq!(l, "a"),
            _ => panic!("premier message inattendu"),
        }
        match rx.try_recv() {
            Ok(Msg::Done { kind, ok, detail }) => {
                assert_eq!(kind, JobKind::Gc);
                assert!(ok);
                assert_eq!(detail, "ok");
            }
            _ => panic!("second message inattendu"),
        }
    }
}
