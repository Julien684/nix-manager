//! Interaction avec Nix : détection de l'état, exécution des commandes,
//! parseurs purs (testables sans interface ni Nix installé).

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;

/// Options indispensables aux commandes modernes (`nix profile`, `nix search`).
pub const FEATURES: [&str; 2] = ["--extra-experimental-features", "nix-command flakes"];

/// Emplacement proposé par l'installeur officiel.
pub const INSTALLER_URL: &str = "https://nixos.org/nix/install";

/// Mode d'installation détecté.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Démon multi-utilisateur (`nix-daemon` présent).
    Daemon,
    /// Installation mono-utilisateur (magasin dans /nix, pas de démon).
    SingleUser,
    /// Rien d'installé.
    Absent,
    /// Magasin ou binaire présent, mode indéterminé (installation cassée).
    Unknown,
}

/// Photographie de l'état de Nix sur la machine.
#[derive(Debug, Clone)]
pub struct NixState {
    pub nix_bin: Option<String>,
    pub version: Option<String>,
    pub daemon: bool,
    pub mode: Mode,
    pub store: bool,
    /// Octets réellement occupés par /nix (`du -sb`).
    pub store_bytes: Option<u64>,
    /// Octets utilisés / total du système de fichiers qui porte /nix.
    pub fs_used: Option<u64>,
    pub fs_total: Option<u64>,
    pub profile: String,
    pub profile_exists: bool,
    /// True si les namespaces utilisateur non privilégiés sont disponibles.
    pub userns: bool,
    /// True si /etc/nix/nix.conf contient déjà `sandbox = false`.
    pub sandbox_off: bool,
    /// Présence de l'outil de désinstallation officiel.
    pub installer: bool,
    pub desktop_count: usize,
    pub xdg_conf: bool,
    pub path_ok: bool,
    pub pkexec: bool,
    /// La racine (et donc /nix) est-elle sur btrfs ?
    pub btrfs: bool,
    /// /nix est-il un sous-volume btrfs dédié ?
    pub nix_subvol: bool,
    /// Paquets non libres autorisés (~/.config/nixpkgs/config.nix).
    pub unfree: bool,
    /// Une opération btrfs est possible (btrfs-progs présent).
    pub btrfs_progs: bool,
}

/// Un résultat de recherche nixpkgs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Attribut installable (`hello`, `python3Packages.requests`, …).
    pub attr: String,
    pub version: String,
    pub description: String,
}

/// Une entrée du profil utilisateur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileEntry {
    pub name: String,
    pub source: String,
}

// ------------------------------------------------------------------
// Helpers système
// ------------------------------------------------------------------

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root"))
}

fn exists(p: &str) -> bool {
    Path::new(p).exists()
}

/// Exécute une commande et renvoie sa sortie standard si elle réussit.
pub fn capture(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn ok_status(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Exécute une commande en diffusant chaque ligne de sortie (stdout + stderr)
/// au journal, dans l'ordre d'arrivée.
pub fn run_streaming(command: &mut Command, log: &mut dyn FnMut(String)) -> bool {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            log(format!("Échec du lancement : {e}"));
            return false;
        }
    };
    let (tx, rx) = mpsc::channel::<String>();
    let mut readers = Vec::new();
    if let Some(o) = child.stdout.take() {
        let t = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in std::io::BufReader::new(o).lines().map_while(Result::ok) {
                let _ = t.send(line);
            }
        }));
    }
    if let Some(e) = child.stderr.take() {
        let t = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in std::io::BufReader::new(e).lines().map_while(Result::ok) {
                let _ = t.send(line);
            }
        }));
    }
    drop(tx);
    for line in rx {
        log(line);
    }
    for r in readers {
        let _ = r.join();
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Exécute une commande en capturant sa sortie standard et en diffusant sa
/// sortie d'erreur au journal (utile pour `nix search --json`, dont le JSON ne
/// doit pas être mélangé à la progression).
pub fn capture_streaming(
    command: &mut Command,
    log: &mut dyn FnMut(String),
) -> Result<String, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("lancement : {e}"))?;
    let (tx, rx) = mpsc::channel::<String>();
    if let Some(e) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(e).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }
    let mut out = String::new();
    if let Some(o) = child.stdout.take() {
        let mut reader = std::io::BufReader::new(o);
        let mut buf = String::new();
        loop {
            match reader.read_line(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    out.push_str(&buf);
                    buf.clear();
                    while let Ok(line) = rx.try_recv() {
                        log(line);
                    }
                }
            }
        }
    }
    while let Ok(line) = rx.try_recv() {
        log(line);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(out)
    } else {
        Err(match status.code() {
            Some(c) => format!("code {c}"),
            None => "interrompu".to_string(),
        })
    }
}

// ------------------------------------------------------------------
// Résolution des binaires
// ------------------------------------------------------------------

/// Chemin du binaire `nix`, en privilégiant les installations Nix.
pub fn nix_bin() -> Option<String> {
    let mut candidates = vec![
        "/nix/var/nix/profiles/default/bin/nix".to_string(),
        format!("{}/.nix-profile/bin/nix", home().display()),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join("nix").to_string_lossy().into_owned());
        }
    }
    candidates.into_iter().find(|p| exists(p))
}

/// Chemin d'un outil livré à côté de `nix` (`nix-collect-garbage`, …).
pub fn sibling_tool(name: &str) -> String {
    if let Some(bin) = nix_bin() {
        if let Some(dir) = Path::new(&bin).parent() {
            let candidate = dir.join(name);
            if candidate.exists() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    name.to_string()
}

/// Commande `nix` avec les options expérimentales déjà posées.
pub fn nix_command(bin: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(FEATURES).args(args);
    cmd
}

// ------------------------------------------------------------------
// Détection
// ------------------------------------------------------------------

pub fn detect() -> NixState {
    let nix_bin = nix_bin();
    let version = nix_bin
        .as_deref()
        .and_then(|b| capture(b, &["--version"]))
        .and_then(|out| parse_version(&out));
    let daemon = ok_status("systemctl", &["is-active", "nix-daemon"]);
    let store = exists("/nix");
    let mode = if daemon || exists("/nix/var/nix/daemon-socket/socket") {
        Mode::Daemon
    } else if store && version.is_some() {
        Mode::SingleUser
    } else if store || version.is_some() {
        Mode::Unknown
    } else {
        Mode::Absent
    };
    let (fs_used, fs_total) = if store { fs_size() } else { (None, None) };
    let store_bytes = if store { dir_size() } else { None };
    let link = home().join(".nix-profile");
    let profile_exists = link.exists();
    let profile = std::fs::read_link(&link)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            format!(
                "{}/.nix-profile → /nix/var/nix/profiles/per-user/{}/profile",
                home().display(),
                std::env::var("USER").unwrap_or_else(|_| "?".into())
            )
        });
    NixState {
        nix_bin,
        version,
        daemon,
        mode,
        store,
        store_bytes,
        fs_used,
        fs_total,
        profile,
        profile_exists,
        userns: userns_ok(),
        sandbox_off: sandbox_disabled(),
        installer: exists("/nix/nix-installer"),
        desktop_count: desktop_entries().len(),
        xdg_conf: home().join(".config/environment.d/60-nix-xdg.conf").exists(),
        path_ok: path_has_nix_profile(),
        pkexec: which("pkexec"),
        btrfs: fs_type("/") == "btrfs",
        nix_subvol: is_subvolume("/nix"),
        unfree: unfree_enabled(),
        btrfs_progs: which("btrfs"),
    }
}

/// Type du système de fichiers qui porte `path` (`stat -f -c %T`).
pub fn fs_type(path: &str) -> String {
    capture("stat", &["-f", "-c", "%T", path]).unwrap_or_default()
}

/// `path` est-il un sous-volume btrfs ?
///
/// `btrfs subvolume show` exige les droits root (il fait une recherche de
/// B-tree) : sans eux il échoue même quand le sous-volume existe. Le nœud
/// racine d'un sous-volume btrfs porte toujours l'inode 256, ce qui donne un
/// test fiable sans privilège.
pub fn is_subvolume(path: &str) -> bool {
    if !Path::new(path).exists() || fs_type(path) != "btrfs" {
        return false;
    }
    if let Ok(out) = Command::new("btrfs")
        .args(["subvolume", "show", path])
        .output()
    {
        if out.status.success() {
            return true;
        }
    }
    capture("stat", &["-c", "%i", path])
        .map(|inode| inode.trim() == "256")
        .unwrap_or(false)
}

/// Que trouve-t-on à l'emplacement `/nix` ?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NixPath {
    /// Rien : la création est directe.
    Absent,
    /// Déjà un sous-volume btrfs.
    Subvolume,
    /// Répertoire vide : `btrfs subvolume create` échoue (« File exists »),
    /// il faut le supprimer d'abord (rmdir) — sans perte.
    EmptyDir,
    /// Répertoire contenant des données (magasin Nix, reste d'installation) :
    /// la conversion en place est impossible sans perdre le magasin.
    NonEmptyDir,
    /// Point de montage : `btrfs subvolume create` échouera aussi.
    Mounted,
}

/// Classe l'occupation actuelle de `/nix`.
pub fn nix_path_state() -> NixPath {
    if !Path::new("/nix").exists() {
        return NixPath::Absent;
    }
    if is_subvolume("/nix") {
        return NixPath::Subvolume;
    }
    if is_mount_point("/nix") {
        return NixPath::Mounted;
    }
    if Path::new("/nix").is_dir() {
        let empty = std::fs::read_dir("/nix")
            .map(|mut it| it.next().is_none())
            .unwrap_or(false);
        if empty {
            return NixPath::EmptyDir;
        }
        return NixPath::NonEmptyDir;
    }
    // Un fichier à la place de /nix : même blocage qu'un répertoire plein.
    NixPath::NonEmptyDir
}

/// `/path` est-il un point de montage distinct ? (lecture de /proc/self/mountinfo)
pub fn is_mount_point(path: &str) -> bool {
    let Ok(mountinfo) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    let wanted = if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    };
    mountinfo.lines().any(|line| {
        // champ 5 = point de montage
        line.split_whitespace().nth(4).map(|mp| mp == wanted).unwrap_or(false)
    })
}

/// Script root qui supprime un répertoire `/nix` vide puis crée le sous-volume.
pub fn subvol_create_script() -> String {
    r#"#!/bin/sh
set -u
if [ -d /nix ] && ! btrfs subvolume show /nix >/dev/null 2>&1; then
  if [ -z "$(ls -A /nix 2>/dev/null)" ]; then
    echo "répertoire /nix vide : suppression puis création du sous-volume"
    rmdir /nix || exit 1
  else
    echo "ERREUR : /nix existe et contient déjà des données ; conversion impossible en place."
    exit 3
  fi
fi
exec btrfs subvolume create /nix
"#
    .to_string()
}

/// `nix-daemon` tourne-t-il ? (appel léger, utilisable depuis un clic)
pub fn daemon_active() -> bool {
    capture("systemctl", &["is-active", "nix-daemon"]).is_some()
}

/// `/etc/zoneinfo` existe-t-il ? Les wrappers Nix (Heroic, Steam) le montent
/// dans leur bac à sable bwrap ; sans lui, le lancement échoue avec
/// « bwrap: Can't find source path /etc/zoneinfo ».
pub fn zoneinfo_ok() -> bool {
    Path::new("/etc/zoneinfo").exists()
}

/// Étape shell idempotente de création du lien (réutilisée par l'installation).
fn zoneinfo_step() -> &'static str {
    r#"if [ ! -e /etc/zoneinfo ] && [ -d /usr/share/zoneinfo ]; then
  ln -s /usr/share/zoneinfo /etc/zoneinfo && echo "lien /etc/zoneinfo créé (compatibilité Steam/Heroic/Lutris)"
fi
"#
}

/// Enveloppe d'installation : crée le lien de compatibilité puis lance
/// l'installeur officiel. Un seul `pkexec`, donc un seul mot de passe.
pub fn install_with_zoneinfo_script(installer: &str) -> String {
    format!(
        "#!/bin/sh\nset -u\n{}exec sh {installer} --daemon --yes\n",
        zoneinfo_step()
    )
}

/// Éléments qui empêchent l'installeur officiel de s'exécuter (il refuse de
/// repartir sur une machine « à moitié » nettoyée, et sort alors sans installer).
#[derive(Debug, Default, Clone)]
pub struct InstallBlockers {
    /// `/etc/*.backup-before-nix` : sauvegardes d'une installation précédente.
    pub backups: Vec<String>,
    /// `/nix` existe déjà (magasin, sous-volume ou répertoire vide).
    pub nix_dir: bool,
    /// Groupe système `nixbld` déjà présent.
    pub nixbld: bool,
    /// Unités systemd `nix-daemon.*` déjà installées.
    pub units: Vec<String>,
}

impl InstallBlockers {
    pub fn any(&self) -> bool {
        !self.backups.is_empty() || self.nix_dir || self.nixbld || !self.units.is_empty()
    }

    /// Liste lisible (chemins et noms, sans phrase) pour le dialogue.
    pub fn describe(&self) -> Vec<String> {
        let mut out: Vec<String> = self.backups.clone();
        if self.nix_dir {
            out.push("/nix".to_string());
        }
        if self.nixbld {
            out.push("groupe nixbld".to_string());
        }
        out.extend(self.units.iter().cloned());
        out
    }
}

/// Détecte les restes qui bloqueraient l'installeur officiel.
pub fn install_blockers() -> InstallBlockers {
    let mut blockers = InstallBlockers::default();
    if let Ok(rd) = std::fs::read_dir("/etc") {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".backup-before-nix") {
                blockers.backups.push(format!("/etc/{name}"));
            }
        }
    }
    blockers.backups.sort();
    blockers.nix_dir = Path::new("/nix").exists();
    blockers.nixbld = Command::new("getent")
        .args(["group", "nixbld"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    for unit in [
        "/etc/systemd/system/nix-daemon.service",
        "/etc/systemd/system/nix-daemon.socket",
    ] {
        if Path::new(unit).exists() {
            blockers.units.push(unit.to_string());
        }
    }
    blockers
}

/// Entrée `.desktop` : seules les clés qui dépendent du chemin nous intéressent.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DesktopEntry {
    pub name: Option<String>,
    pub exec: Option<String>,
    pub try_exec: Option<String>,
    pub icon: Option<String>,
}

/// Extrait `Name`, `Exec`, `TryExec` et `Icon` de la section `[Desktop Entry]`.
pub fn parse_desktop_entry(text: &str) -> DesktopEntry {
    let mut entry = DesktopEntry::default();
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim_end();
        if line.starts_with('[') {
            in_section = line == "[Desktop Entry]";
            continue;
        }
        if !in_section || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim();
            match key.trim() {
                // `Name` sans locale : le libellé de repli, suffisant pour le
                // journal (les variantes Name[fr] sont ignorées).
                "Name" if entry.name.is_none() => entry.name = Some(value.to_string()),
                "Exec" => entry.exec = Some(value.to_string()),
                "TryExec" => entry.try_exec = Some(value.to_string()),
                "Icon" => entry.icon = Some(value.to_string()),
                _ => {}
            }
        }
    }
    entry
}

/// Cherche `name` dans `dir` : correspondance exacte, puis insensible à la
/// casse. Les `.desktop` n'utilisent pas toujours la même casse que le binaire
/// du profil (`Exec=Discord` pour `bin/discord`, cas réel de Discord).
fn find_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.exists() {
        return Some(exact);
    }
    let wanted = name.to_lowercase();
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let candidate = entry.file_name().to_string_lossy().to_lowercase();
        if candidate == wanted {
            Some(entry.path())
        } else {
            None
        }
    })
}

/// Chemin absolu d'un binaire du profil Nix (résolu dans le magasin, donc
/// stable même si le profil change).
pub fn profile_binary(name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') {
        return None;
    }
    for dir in [
        home().join(".nix-profile/bin"),
        PathBuf::from("/nix/var/nix/profiles/default/bin"),
    ] {
        if let Some(found) = find_in_dir(&dir, name) {
            return Some(std::fs::canonicalize(&found).unwrap_or(found));
        }
    }
    None
}

/// Réécrit une valeur `Exec`/`TryExec` : le programme devient absolu quand il
/// existe dans le profil, les arguments (et codes `%U`) sont conservés.
///
/// Indispensable : le `.desktop` de Discord dit `Exec=discord`, or le menu
/// graphique n'a pas `~/.nix-profile/bin` dans son PATH avant reconnexion —
/// le lancement échoue alors silencieusement.
pub fn rewrite_exec(value: &str) -> String {
    rewrite_exec_with(value, &profile_binary)
}

/// Variante de [`rewrite_exec`] avec un résolveur injecté (testable sans profil).
pub fn rewrite_exec_with(value: &str, resolve: &dyn Fn(&str) -> Option<PathBuf>) -> String {
    let mut parts = value.split_whitespace();
    let Some(program) = parts.next() else {
        return value.to_string();
    };
    let rest: Vec<&str> = parts.collect();
    let resolved = if program.starts_with('/') {
        None
    } else {
        resolve(program).map(|p| p.to_string_lossy().into_owned())
    };
    match resolved {
        Some(abs) if rest.is_empty() => abs,
        Some(abs) => format!("{abs} {}", rest.join(" ")),
        None => value.to_string(),
    }
}

/// Chemin absolu de l'icône `name` dans le profil Nix.
///
/// `Icon=discord` ne se résout que si `~/.nix-profile/share` est dans
/// `XDG_DATA_DIRS` : on pointe donc le fichier réel, ce qui rend l'icône
/// visible immédiatement dans le menu.
pub fn profile_icon(name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name.starts_with('/') || name.contains('/') {
        return None;
    }
    let share = home().join(".nix-profile/share");
    let mut candidates: Vec<PathBuf> = Vec::new();
    for size in ["256x256", "128x128", "64x64", "48x48", "32x32", "scalable"] {
        for ext in ["png", "svg", "xpm"] {
            candidates.push(
                share
                    .join("icons/hicolor")
                    .join(size)
                    .join("apps")
                    .join(format!("{name}.{ext}")),
            );
        }
    }
    for ext in ["png", "svg", "xpm"] {
        candidates.push(share.join("pixmaps").join(format!("{name}.{ext}")));
    }
    candidates
        .into_iter()
        .find(|p| p.exists())
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
}

/// Marqueur ajouté aux fichiers générés (permet de les distinguer des entrées
/// installées par eopkg et de nettoyer les périmés).
pub const DESKTOP_MARKER: &str = "# Généré par Nix Manager";

/// Réécrit un `.desktop` pour le menu : `Exec`, `TryExec` et `Icon` en chemins
/// absolus. Renvoie le contenu et `true` si au moins une clé a changé.
pub fn rewrite_desktop_file(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len() + 64);
    let mut changed = false;
    for line in text.lines() {
        let trimmed = line.trim_end();
        let replacement = if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "Exec" => {
                    let new = rewrite_exec(value);
                    if new != value {
                        changed = true;
                    }
                    Some(format!("Exec={new}"))
                }
                "TryExec" => {
                    let new = rewrite_exec(value);
                    if new != value {
                        changed = true;
                    }
                    Some(format!("TryExec={new}"))
                }
                "Icon" => match profile_icon(value) {
                    Some(path) => {
                        changed = true;
                        Some(format!("Icon={}", path.display()))
                    }
                    None => None,
                },
                _ => None,
            }
        } else {
            None
        };
        out.push_str(replacement.as_deref().unwrap_or(trimmed));
        out.push('\n');
    }
    out.push_str(DESKTOP_MARKER);
    out.push('\n');
    (out, changed)
}

/// Sous-commande d'ajout au profil : `add` (Nix récent) ou `install` (ancien
/// nom, désormais déprécié — `nix` imprime alors
/// « warning: 'install' is a deprecated alias for 'add' »).
///
/// Le choix est fait une fois, en interrogeant le binaire : `profile add --help`
/// n'existe pas sur les Nix antérieurs, qui doivent garder `install`.
pub fn profile_add_subcommand(bin: &str) -> &'static str {
    static CHOSEN: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    CHOSEN.get_or_init(|| {
        let has_add = capture(
            bin,
            &[
                "--extra-experimental-features",
                "nix-command flakes",
                "profile",
                "add",
                "--help",
            ],
        )
        .is_some();
        if has_add {
            "add"
        } else {
            "install"
        }
    })
}

/// Idem pour le retrait : `remove` (ancien) ou `delete` si le binaire ne
/// connaît plus que ce dernier.
pub fn profile_remove_subcommand(bin: &str) -> &'static str {
    static CHOSEN: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    CHOSEN.get_or_init(|| {
        let has_remove = capture(
            bin,
            &[
                "--extra-experimental-features",
                "nix-command flakes",
                "profile",
                "remove",
                "--help",
            ],
        )
        .is_some();
        if has_remove {
            "remove"
        } else {
            "delete"
        }
    })
}

/// Chemin du fichier de configuration nixpkgs de l'utilisateur.
pub fn nixpkgs_config_path() -> PathBuf {
    home().join(".config/nixpkgs/config.nix")
}

/// Contenu minimal de `~/.config/nixpkgs/config.nix` autorisant les paquets
/// non libres (licence `unfree`, cas de `discord`, `steam`, …).
pub const UNFREE_CONFIG: &str = "{ allowUnfree = true; }\n";

/// Les paquets non libres sont-ils autorisés pour cet utilisateur ?
pub fn unfree_enabled() -> bool {
    match std::fs::read_to_string(nixpkgs_config_path()) {
        Ok(conf) => conf
            .lines()
            .map(|l| l.split('#').next().unwrap_or(""))
            .any(|l| {
                let l = l.replace(' ', "");
                l.contains("allowUnfree") && l.contains("true")
            }),
        Err(_) => false,
    }
}

/// Espace utilisé/total du système de fichiers qui porte `/nix` (via `stat -f`,
/// aucune dépendance C).
fn fs_size() -> (Option<u64>, Option<u64>) {
    match capture("stat", &["-f", "-c", "%b %f %S", "/nix"])
        .and_then(|out| parse_stat_f(&out))
    {
        Some((used, total)) => (Some(used), Some(total)),
        None => (None, None),
    }
}

/// Taille réelle du magasin (`du -sb /nix`), en octets.
fn dir_size() -> Option<u64> {
    capture("du", &["-sb", "/nix"]).and_then(|out| parse_du_s(&out))
}

/// Namespaces utilisateur non privilégiés disponibles ?
fn userns_ok() -> bool {
    let max = std::fs::read_to_string("/proc/sys/user/max_user_namespaces")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    if let Some(max) = max {
        if max == 0 {
            return false;
        }
    }
    // Présent sur Debian et dérivés : 0 = désactivé.
    if let Ok(clone) = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone") {
        if clone.trim() == "0" {
            return false;
        }
    }
    true
}

/// `/etc/nix/nix.conf` désactive-t-il déjà le bac à sable ?
fn sandbox_disabled() -> bool {
    match std::fs::read_to_string("/etc/nix/nix.conf") {
        Ok(conf) => conf.lines().any(|l| {
            let l = l.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = l.split_once('=') else {
                return false;
            };
            k.trim() == "sandbox" && v.trim().eq_ignore_ascii_case("false")
        }),
        Err(_) => false,
    }
}

fn path_has_nix_profile() -> bool {
    std::env::var_os("PATH")
        .map(|p| {
            let p = p.to_string_lossy();
            p.contains("/nix/var/nix/profiles/default/bin") || p.contains(".nix-profile/bin")
        })
        .unwrap_or(false)
}

/// `true` si la commande existe dans le PATH.
pub fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(name).exists()))
        .unwrap_or(false)
}

/// Fichiers `.desktop` fournis par le profil utilisateur.
pub fn desktop_entries() -> Vec<PathBuf> {
    let dir = home().join(".nix-profile/share/applications");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|e| e == "desktop").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

// ------------------------------------------------------------------
// Parseurs purs (testés unitairement)
// ------------------------------------------------------------------

/// `nix (Nix) 2.30.1` → `2.30.1`
pub fn parse_version(out: &str) -> Option<String> {
    let line = out.lines().next()?.trim();
    line.split_whitespace()
        .last()
        .filter(|v| v.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false))
        .map(|v| v.to_string())
}

/// `du -sb /nix` → octets (`1234\t/nix`).
pub fn parse_du_s(out: &str) -> Option<u64> {
    out.split_whitespace().next()?.parse().ok()
}

/// `stat -f -c '%b %f %S'` → (octets utilisés, octets totaux).
pub fn parse_stat_f(out: &str) -> Option<(u64, u64)> {
    let mut it = out.split_whitespace();
    let blocks: u64 = it.next()?.parse().ok()?;
    let free: u64 = it.next()?.parse().ok()?;
    let bsize: u64 = it.next()?.parse().ok()?;
    if blocks == 0 || bsize == 0 {
        return None;
    }
    let total = blocks.saturating_mul(bsize);
    let used = blocks.saturating_sub(free).saturating_mul(bsize);
    Some((used, total))
}

/// `legacyPackages.x86_64-linux.hello` → `hello`
pub fn attr_from_key(key: &str) -> String {
    if let Some(rest) = key.strip_prefix("legacyPackages.") {
        if let Some((_system, tail)) = rest.split_once('.') {
            return tail.to_string();
        }
    }
    key.to_string()
}

/// Pertinence d'un attribut pour un terme recherché (0 = meilleur).
///
/// `nix search` renvoie ses résultats par pertinence, mais `serde_json` les
/// rend dans un `BTreeMap` trié : sans ce score, « hello » classait
/// `akkuPackages.retropixel-hello` avant `hello`.
fn relevance(attr: &str, term: &str) -> u8 {
    let attr_l = attr.to_lowercase();
    let term_l = term.to_lowercase();
    let last = attr_l.rsplit('.').next().unwrap_or(&attr_l);
    if attr_l == term_l || last == term_l {
        0
    } else if last.starts_with(&term_l) {
        1
    } else if attr_l.starts_with(&term_l) {
        2
    } else if last.contains(&term_l) {
        3
    } else {
        4
    }
}

/// Résultats de `nix search nixpkgs <terme> --json`, classés puis tronqués.
pub fn parse_search_json(json: &str, limit: usize, term: &str) -> Vec<Hit> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(obj) = value.as_object() else {
        return Vec::new();
    };
    let mut hits: Vec<Hit> = obj
        .iter()
        .map(|(key, val)| Hit {
            attr: attr_from_key(key),
            version: val
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            description: val
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .replace(['\n', '\r'], " ")
                .trim()
                .to_string(),
        })
        .collect();
    hits.sort_by(|a, b| {
        relevance(&a.attr, term)
            .cmp(&relevance(&b.attr, term))
            // À pertinence égale, l'attribut le moins imbriqué gagne :
            // `hello` avant `haskellPackages.hello`.
            .then_with(|| {
                a.attr.matches('.').count().cmp(&b.attr.matches('.').count())
            })
            .then_with(|| a.attr.cmp(&b.attr))
    });
    hits.truncate(limit);
    hits
}

/// `nix profile list --json` → entrées triées.
///
/// Deux formes sont acceptées : `{"elements": {...}, "version": 2}` (Nix ≥ 2.19)
/// et une simple table `nom → objet` (binaires antérieurs).
pub fn parse_profile_json(json: &str) -> Vec<ProfileEntry> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let map = value
        .get("elements")
        .and_then(|e| e.as_object())
        .or_else(|| value.as_object())
        .cloned()
        .unwrap_or_default();
    let mut out: Vec<ProfileEntry> = map
        .iter()
        .map(|(name, entry)| ProfileEntry {
            name: name.clone(),
            source: entry
                .get("originalUrl")
                .or_else(|| entry.get("uri"))
                .or_else(|| entry.get("url"))
                .or_else(|| entry.get("attrPath"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Une référence de flake est-elle utilisable telle quelle ?
pub fn flake_ref_ok(input: &str) -> bool {
    let s = input.trim();
    if s.is_empty() || s.len() > 300 {
        return false;
    }
    if s.chars()
        .any(|c| c.is_whitespace() || ";&|`$<>\\\"'*?{}[]()".contains(c))
    {
        return false;
    }
    s.chars().any(|c| c.is_ascii_alphanumeric())
}

/// Cible passée à `nix profile install`.
///
/// Un simple nom d'attribut (`hello`) est préfixé par `nixpkgs#` ; toute
/// référence explicite (`nixpkgs#hello`, `github:u/r`, `./dossier`, URL) est
/// conservée telle quelle.
pub fn install_target(input: &str) -> String {
    let s = input.trim();
    if s.contains('#') || s.contains(':') || s.contains('/') || s.starts_with('.') {
        s.to_string()
    } else {
        format!("nixpkgs#{s}")
    }
}

/// Taille lisible (o, Kio, Mio, Gio, Tio).
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["o", "Kio", "Mio", "Gio", "Tio"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Contenu du script de désinstallation manuelle (sans l'outil officiel).
pub fn manual_uninstall_script() -> String {
    r#"#!/bin/sh
set -u
echo "== Arrêt et suppression du service nix-daemon =="
systemctl stop nix-daemon.socket nix-daemon.service 2>/dev/null || true
systemctl disable nix-daemon.socket nix-daemon.service 2>/dev/null || true
rm -f /etc/systemd/system/nix-daemon.service \
      /etc/systemd/system/nix-daemon.socket \
      /etc/systemd/system/multi-user.target.wants/nix-daemon.service \
      /etc/systemd/system/sockets.target.wants/nix-daemon.socket
systemctl daemon-reload 2>/dev/null || true
echo "== Suppression des utilisateurs du groupe nixbld =="
n=1
while [ "$n" -le 32 ]; do
  userdel -r "nixbld$n" 2>/dev/null || true
  n=$((n + 1))
done
groupdel nixbld 2>/dev/null || true
echo "== Suppression des fichiers d'environnement =="
rm -f /etc/profile.d/nix.sh /etc/profile.d/nix-daemon.sh
# L'installeur encadre son bloc par « # Nix » … « # End Nix » : supprimer le
# bloc entier, sinon il reste un « fi » orphelin (erreur de syntaxe pour tout
# shell de connexion). Les deux motifs suivants nettoient d'éventuels restes.
for f in /etc/bashrc /etc/zshrc /etc/profile /etc/bash.bashrc; do
  if [ -f "$f" ]; then
    sed -i -e '/^# Nix$/,/^# End Nix$/d' -e '/\/nix\/var\/nix/d' -e '/nix-daemon/d' "$f" 2>/dev/null || true
  fi
done
rm -f /etc/sudoers.d/nix-sudoers
for f in /etc/sudoers.d/nix*; do
  [ -e "$f" ] && rm -f "$f"
done
# Sauvegardes créées par l'installeur : sans leur suppression, une
# réinstallation s'arrête (« /etc/bashrc.backup-before-nix exists »).
rm -f /etc/bashrc.backup-before-nix /etc/zshrc.backup-before-nix \
      /etc/bash.bashrc.backup-before-nix /etc/profile.backup-before-nix
echo "== Suppression du magasin et de la configuration =="
rm -rf /nix
rm -rf /etc/nix
echo "== Terminé =="
"#
    .to_string()
}

/// Script root qui force `sandbox = false` puis redémarre le démon.
pub fn sandbox_script() -> String {
    r#"#!/bin/sh
set -u
mkdir -p /etc/nix
touch /etc/nix/nix.conf
if grep -qE '^[[:space:]]*sandbox[[:space:]]*=' /etc/nix/nix.conf; then
  sed -i -E 's#^[[:space:]]*sandbox[[:space:]]*=.*#sandbox = false#' /etc/nix/nix.conf
else
  printf '\nsandbox = false\n' >> /etc/nix/nix.conf
fi
echo "sandbox = false écrit dans /etc/nix/nix.conf"
systemctl restart nix-daemon 2>/dev/null && echo "nix-daemon redémarré" || echo "nix-daemon non redémarré"
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsee() {
        assert_eq!(parse_version("nix (Nix) 2.30.1\n"), Some("2.30.1".into()));
        assert_eq!(parse_version("nix 2.18.1"), Some("2.18.1".into()));
        assert_eq!(parse_version("commande inconnue"), None);
    }

    #[test]
    fn du_s_parse() {
        assert_eq!(parse_du_s("4812345\t/nix\n"), Some(4_812_345));
        assert_eq!(parse_du_s(""), None);
        assert_eq!(parse_du_s("erreur"), None);
    }

    #[test]
    fn stat_f_parse() {
        // 100 blocs de 4096 octets, 40 libres → 60 utilisés.
        assert_eq!(parse_stat_f("100 40 4096\n"), Some((60 * 4096, 100 * 4096)));
        assert_eq!(parse_stat_f("0 0 4096"), None);
        assert_eq!(parse_stat_f("n'importe quoi"), None);
    }

    #[test]
    fn attribut_depuis_cle() {
        assert_eq!(attr_from_key("legacyPackages.x86_64-linux.hello"), "hello");
        assert_eq!(
            attr_from_key("legacyPackages.aarch64-linux.python3Packages.requests"),
            "python3Packages.requests"
        );
        assert_eq!(attr_from_key("hello"), "hello");
    }

    #[test]
    fn recherche_parse_et_tronque() {
        let json = r#"{
            "legacyPackages.x86_64-linux.hello": {"pname":"hello","version":"2.12.1","description":"A program that produces a familiar, friendly greeting"},
            "legacyPackages.x86_64-linux.hello-wayland": {"pname":"hello-wayland","version":"0.1","description":"Hello world\nWayland client"}
        }"#;
        let hits = parse_search_json(json, 10, "hello");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].attr, "hello");
        assert_eq!(hits[0].version, "2.12.1");
        assert!(hits[0].description.contains("friendly greeting"));
        assert_eq!(hits[1].description, "Hello world Wayland client");
        assert_eq!(parse_search_json(json, 1, "hello").len(), 1);
        assert!(parse_search_json("pas du json", 10, "hello").is_empty());
    }

    #[test]
    fn recherche_classe_par_pertinence() {
        // Cas réel : « hello » doit sortir `hello` avant
        // `akkuPackages.retropixel-hello` (l'ordre alphabétique les inversait).
        let json = r#"{
            "legacyPackages.x86_64-linux.akkuPackages.retropixel-hello": {"pname":"retropixel-hello","version":"1.2.3","description":"Akku Hello"},
            "legacyPackages.x86_64-linux.hello": {"pname":"hello","version":"2.12","description":"A friendly greeting"},
            "legacyPackages.x86_64-linux.hello-wayland": {"pname":"hello-wayland","version":"0.1","description":"Wayland"},
            "legacyPackages.x86_64-linux.haskellPackages.hello": {"pname":"hello","version":"1.0","description":"Haskell hello"}
        }"#;
        let hits = parse_search_json(json, 10, "hello");
        assert_eq!(hits[0].attr, "hello", "correspondance exacte d'abord");
        assert_eq!(hits[1].attr, "haskellPackages.hello", "moins imbriqué que les autres homonymes");
        assert_eq!(hits[2].attr, "hello-wayland", "préfixe ensuite");
        assert_eq!(hits[3].attr, "akkuPackages.retropixel-hello");
    }

    #[test]
    fn profil_parse_deux_formes() {
        let moderne = r#"{"version":2,"elements":{
            "hello":{"active":true,"attrPath":"legacyPackages.x86_64-linux.hello","originalUrl":"flake:nixpkgs","storePaths":["/nix/store/x"]}}}"#;
        let entries = parse_profile_json(moderne);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "hello");
        assert_eq!(entries[0].source, "flake:nixpkgs");

        let ancien = r#"{"hello":{"uri":"flake:nixpkgs#hello"}}"#;
        let entries = parse_profile_json(ancien);
        assert_eq!(entries[0].source, "flake:nixpkgs#hello");

        assert!(parse_profile_json("").is_empty());
    }

    #[test]
    fn references_de_flake() {
        assert!(flake_ref_ok("nixpkgs#hello"));
        assert!(flake_ref_ok("github:NixOS/nixpkgs"));
        assert!(flake_ref_ok("./mon-flake"));
        assert!(!flake_ref_ok(""));
        assert!(!flake_ref_ok("hello; rm -rf /"));
        assert!(!flake_ref_ok("a b"));
        assert!(!flake_ref_ok("$(whoami)"));
    }

    #[test]
    fn cible_install() {
        assert_eq!(install_target("hello"), "nixpkgs#hello");
        assert_eq!(install_target("nixpkgs#hello"), "nixpkgs#hello");
        assert_eq!(install_target("github:u/r"), "github:u/r");
        assert_eq!(install_target("./flake"), "./flake");
        assert_eq!(install_target("  hello  "), "nixpkgs#hello");
    }

    #[test]
    fn script_desinstallation_supprime_le_bloc_entier() {
        let s = manual_uninstall_script();
        // Sans la suppression du bloc « # Nix » … « # End Nix », il resterait
        // un « fi » orphelin dans /etc/bashrc (cf. test sur VM Solus).
        assert!(s.contains("/^# Nix$/,/^# End Nix$/d"));
        for must in [
            "nix-daemon.service",
            "nix-daemon.socket",
            "userdel -r",
            "groupdel nixbld",
            "rm -rf /nix",
            "rm -rf /etc/nix",
            "/etc/sudoers.d",
            ".backup-before-nix",
        ] {
            assert!(s.contains(must), "motif absent : {must}");
        }
    }

    #[test]
    fn entree_desktop_parse() {
        let text = "[Desktop Entry]\nType=Application\nName=Discord\nExec=discord %U\nTryExec=discord\nIcon=discord\n\n[Desktop Action new]\nName=New\nExec=discord --new\n";
        let e = parse_desktop_entry(text);
        assert_eq!(e.exec.as_deref(), Some("discord %U"));
        assert_eq!(e.try_exec.as_deref(), Some("discord"));
        assert_eq!(e.icon.as_deref(), Some("discord"));
    }

    #[test]
    fn reecriture_exec_absolue_et_arguments_conserves() {
        let finder = |name: &str| -> Option<std::path::PathBuf> {
            if name == "discord" {
                Some(std::path::PathBuf::from("/nix/store/abc-discord/bin/Discord"))
            } else {
                None
            }
        };
        assert_eq!(
            rewrite_exec_with("discord %U", &finder),
            "/nix/store/abc-discord/bin/Discord %U"
        );
        assert_eq!(rewrite_exec_with("discord", &finder), "/nix/store/abc-discord/bin/Discord");
        // Déjà absolu : inchangé. Inconnu : inchangé.
        assert_eq!(rewrite_exec_with("/usr/bin/x %f", &finder), "/usr/bin/x %f");
        assert_eq!(rewrite_exec_with("inconnu --flag", &finder), "inconnu --flag");
    }

    #[test]
    fn script_zoneinfo_idempotent() {
        let s = install_with_zoneinfo_script("/tmp/inst.sh");
        assert!(s.contains("ln -s /usr/share/zoneinfo /etc/zoneinfo"));
        // Idempotent : ne recrée pas le lien s'il existe déjà, et ne casse rien
        // si la source manque.
        assert!(s.contains("[ ! -e /etc/zoneinfo ]"));
        assert!(!s.contains("exit 3"));
    }

    #[test]
    fn installation_cree_le_lien_zoneinfo_avant_l_installeur() {
        let s = install_with_zoneinfo_script("/tmp/nix-install-1.sh");
        let link = s.find("ln -s /usr/share/zoneinfo /etc/zoneinfo").unwrap();
        let exec = s.find("exec sh /tmp/nix-install-1.sh --daemon --yes").unwrap();
        assert!(link < exec, "le lien doit précéder l'installeur");
    }

    #[test]
    fn blocages_d_installation_listes() {
        let b = InstallBlockers {
            backups: vec!["/etc/bashrc.backup-before-nix".to_string()],
            nix_dir: true,
            nixbld: true,
            units: vec!["/etc/systemd/system/nix-daemon.service".to_string()],
        };
        assert!(b.any());
        assert_eq!(b.describe().len(), 4);
        let vide = InstallBlockers::default();
        assert!(!vide.any());
        assert!(vide.describe().is_empty());
    }

    #[test]
    fn binaire_trouve_malgre_la_casse() {
        let dir = std::env::temp_dir().join(format!("nixmgr-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("discord"), b"#!/bin/sh\n").unwrap();

        // `Exec=Discord` (majuscule) doit résoudre vers `bin/discord`.
        assert!(find_in_dir(&dir, "Discord").is_some());
        assert!(find_in_dir(&dir, "discord").is_some());
        assert!(find_in_dir(&dir, "absent").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reecriture_desktop_ajoute_le_marqueur() {
        // Sans profil Nix installé, les valeurs restent telles quelles mais le
        // fichier est marqué (base du nettoyage des entrées périmées).
        let (out, changed) = rewrite_desktop_file(
            "[Desktop Entry]\nExec=discord %U\nIcon=discord\nName=Discord\n",
        );
        assert!(out.contains("Name=Discord"));
        assert!(out.ends_with(&format!("{DESKTOP_MARKER}\n")));
        assert!(!changed);
    }

    #[test]
    fn script_sous_volume_gere_le_repertoire_vide() {
        let s = subvol_create_script();
        // Sans le rmdir préalable, `btrfs subvolume create` répond
        // « File exists » sur un répertoire vide existant (cas réel : VM où
        // /nix avait été créé sans sous-volume).
        assert!(s.contains("ls -A /nix"));
        assert!(s.contains("rmdir /nix"));
        assert!(s.contains("btrfs subvolume create /nix"));
        assert!(s.contains("exit 3"), "doit refuser un /nix non vide");
    }

    #[test]
    fn racine_est_un_point_de_montage() {
        assert!(is_mount_point("/"));
        assert!(!is_mount_point("/nix-inexistant-xyz"));
    }

    #[test]
    fn taille_lisible_et_sandbox() {
        assert_eq!(human(512), "512 o");
        assert_eq!(human(2048), "2.0 Kio");
        assert_eq!(human(3 * 1024 * 1024), "3.0 Mio");
        assert_eq!(human(5 * 1024 * 1024 * 1024), "5.0 Gio");
    }
}
