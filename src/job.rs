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
                let hits = nix::parse_search_json(&json, 200);
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

pub fn spawn_install_nix(tx: Sender<Msg>) {
    std::thread::spawn(move || {
        if nix::nix_bin().is_some() {
            done(
                &tx,
                JobKind::InstallNix,
                false,
                "Nix est déjà installé".to_string(),
            );
            return;
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
        let ok = pkexec_stream(&tx, "sh", &[script.as_str(), "--daemon", "--yes"]);
        let _ = std::fs::remove_file(&script);
        if !ok {
            done(
                &tx,
                JobKind::InstallNix,
                false,
                "l'installeur a échoué (voir le journal)".to_string(),
            );
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

pub fn spawn_pkg_install(tx: Sender<Msg>, target: String) {
    std::thread::spawn(move || {
        let ok = run_nix(&tx, JobKind::InstallPkg, &["profile", "install", &target]);
        done(
            &tx,
            JobKind::InstallPkg,
            ok,
            if ok { target } else { format!("échec : {target}") },
        );
    });
}

pub fn spawn_pkg_remove(tx: Sender<Msg>, name: String) {
    std::thread::spawn(move || {
        let ok = run_nix(&tx, JobKind::RemovePkg, &["profile", "remove", &name]);
        done(
            &tx,
            JobKind::RemovePkg,
            ok,
            if ok { name } else { format!("échec : {name}") },
        );
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
        let mut linked = 0;
        let mut failed = 0;
        if let Err(e) = std::fs::create_dir_all(&apps_dir) {
            done(&tx, JobKind::Desktop, false, format!("{} : {e}", apps_dir.display()));
            return;
        }
        for src in nix::desktop_entries() {
            let Some(name) = src.file_name() else { continue };
            let dst = apps_dir.join(name);
            // Remplace un lien existant (y compris cassé).
            let _ = std::fs::remove_file(&dst);
            match std::os::unix::fs::symlink(&src, &dst) {
                Ok(()) => linked += 1,
                Err(e) => {
                    failed += 1;
                    log(&tx, format!("échec du lien {} : {e}", dst.display()));
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
                "{linked} lien(s) .desktop, environment.d : {}",
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

#[cfg(test)]
mod tests {
    use super::*;

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
