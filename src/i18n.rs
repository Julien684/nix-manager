//! Internationalisation (français / anglais).
//!
//! Aucune dépendance externe : chaque langue est un jeu de textes `&'static str`
//! et [`fill`] remplace les `{}` dans l'ordre. La langue suit la locale du
//! système (`NIX_MANAGER_LANG`, puis `LC_ALL`, `LC_MESSAGES`, `LANG`, puis
//! `/etc/locale.conf`), l'anglais servant de repli.

/// Langue de l'interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Fr,
    En,
}

impl Lang {
    /// Langue à utiliser : variable d'environnement dédiée, sinon locale.
    pub fn detect() -> Lang {
        if let Some(value) = std::env::var_os("NIX_MANAGER_LANG") {
            let value = value.to_string_lossy();
            if !value.trim().is_empty() {
                return parse_lang(&value);
            }
        }
        for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Some(value) = std::env::var_os(var) {
                let value = value.to_string_lossy();
                if !value.trim().is_empty() {
                    return from_locale(&value);
                }
            }
        }
        // Session sans locale (ssh non interactif, cron…) : on lit la
        // configuration système.
        if let Ok(conf) = std::fs::read_to_string("/etc/locale.conf") {
            for line in conf.lines() {
                let line = line.trim();
                for key in ["LANG=", "LC_MESSAGES="] {
                    if let Some(value) = line.strip_prefix(key) {
                        let value = value.trim().trim_matches('"');
                        if !value.is_empty() {
                            return from_locale(value);
                        }
                    }
                }
            }
        }
        Lang::En
    }

    /// Jeu de textes de cette langue.
    pub fn texts(self) -> &'static Texts {
        match self {
            Lang::Fr => &FR,
            Lang::En => &EN,
        }
    }
}

/// Interprète un nom de langue explicite (`fr`, `en`, `français`, …).
fn parse_lang(value: &str) -> Lang {
    match value.trim().to_ascii_lowercase().as_str() {
        "fr" | "fra" | "french" | "francais" | "français" => Lang::Fr,
        _ => Lang::En,
    }
}

/// Interprète une valeur de locale (`fr_FR.UTF-8`, `en_US`, `C`, …).
fn from_locale(value: &str) -> Lang {
    let primary = value.split(['.', '@']).next().unwrap_or("");
    let code = primary.split(['_', '-']).next().unwrap_or("");
    if code.eq_ignore_ascii_case("fr") {
        Lang::Fr
    } else {
        Lang::En
    }
}

/// Remplace les `{}` de `template` par `args`, dans l'ordre.
pub fn fill(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    for arg in args {
        match rest.split_once("{}") {
            Some((before, after)) => {
                out.push_str(before);
                out.push_str(arg);
                rest = after;
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// Tous les textes visibles de l'interface, dans une langue donnée.
pub struct Texts {
    pub app_title: &'static str,
    pub tab_nix: &'static str,
    pub tab_packages: &'static str,
    pub tab_maintenance: &'static str,

    pub menu_file: &'static str,
    pub menu_quit: &'static str,
    pub menu_refresh: &'static str,
    pub menu_help: &'static str,
    pub menu_about: &'static str,
    pub about_title: &'static str,
    pub about_body: &'static str,

    // ---- état de Nix ----
    pub lbl_nix: &'static str,
    pub lbl_daemon: &'static str,
    pub lbl_mode: &'static str,
    pub lbl_profile: &'static str,
    pub lbl_store: &'static str,
    pub lbl_sandbox: &'static str,
    pub lbl_desktop: &'static str,
    pub lbl_path: &'static str,
    pub val_absent: &'static str,
    pub val_active: &'static str,
    pub val_inactive: &'static str,
    pub val_unknown: &'static str,
    pub val_yes: &'static str,
    pub val_no: &'static str,
    pub mode_daemon: &'static str,
    pub mode_single: &'static str,
    pub hint_nix: &'static str,
    pub hint_store: &'static str,
    pub hint_path: &'static str,
    pub hint_discover: &'static str,
    pub btn_refresh_state: &'static str,
    pub btn_install_nix: &'static str,
    pub btn_uninstall_nix: &'static str,

    // ---- paquets ----
    pub lbl_search: &'static str,
    pub btn_search: &'static str,
    pub col_name: &'static str,
    pub col_version: &'static str,
    pub col_desc: &'static str,
    pub col_source: &'static str,
    pub btn_install_sel: &'static str,
    pub lbl_flake: &'static str,
    pub btn_install_flake: &'static str,
    pub lbl_installed: &'static str,
    pub btn_refresh_profile: &'static str,
    pub btn_remove_sel: &'static str,

    // ---- maintenance ----
    pub lbl_maintenance: &'static str,
    pub btn_upgrade: &'static str,
    pub btn_gc: &'static str,
    pub btn_desktop: &'static str,
    pub btn_sandbox: &'static str,

    // ---- divers ----
    pub confirm_title: &'static str,
    pub confirm_uninstall: &'static str,
    pub confirm_gc: &'static str,
    pub confirm_sandbox: &'static str,
    pub status_ready: &'static str,
    pub status_busy: &'static str,
    pub msg_no_nix: &'static str,
    pub msg_no_selection: &'static str,
    pub msg_bad_flake: &'static str,
    pub msg_empty_term: &'static str,
    pub search_hint_first: &'static str,
    pub search_done: &'static str,
    pub state_done: &'static str,
    pub store_line: &'static str,
    pub store_only: &'static str,
    pub desktop_line: &'static str,
    pub profile_line: &'static str,
    pub journal_placeholder: &'static str,
    pub msg_already_installed: &'static str,
    pub sandbox_ok: &'static str,
    pub sandbox_disabled: &'static str,
    pub userns_missing: &'static str,

    // ---- sous-volume btrfs et paquets non libres ----
    pub lbl_subvol: &'static str,
    pub subvol_yes: &'static str,
    pub subvol_no: &'static str,
    pub subvol_na: &'static str,
    pub btn_subvol_create: &'static str,
    pub btn_subvol_delete: &'static str,
    pub confirm_subvol_delete: &'static str,
    pub btn_unfree_on: &'static str,
    pub btn_unfree_off: &'static str,
    pub msg_no_btrfs: &'static str,
    pub msg_subvol_exists: &'static str,
    pub msg_subvol_missing: &'static str,
    pub hint_subvol: &'static str,
    pub hint_unfree: &'static str,
}

static FR: Texts = Texts {
    app_title: "Nix Manager",
    tab_nix: "Nix",
    tab_packages: "Paquets",
    tab_maintenance: "Maintenance",

    menu_file: "Fichier",
    menu_quit: "Quitter",
    menu_refresh: "Actualiser l'état",
    menu_help: "Aide",
    menu_about: "À propos",
    about_title: "À propos de Nix Manager",
    about_body: "Nix Manager — gérer Nix et ses paquets sur Solus.\n\n\
Installe et désinstalle Nix (mode démon multi-utilisateur), recherche dans\n\
nixpkgs, installe des paquets ou des flakes dans le profil UTILISATEUR\n\
(sans root), les met à jour et les retire.\n\n\
Les paquets installés par Nix n'apparaissent ni dans Discover ni dans eopkg :\n\
leurs mises à jour passent par Nix, jamais par le gestionnaire de la\n\
distribution.\n\n\
Licence GPL-3.0-or-later.",

    lbl_nix: "Nix :",
    lbl_daemon: "Démon nix-daemon :",
    lbl_mode: "Mode d'installation :",
    lbl_profile: "Profil utilisateur :",
    lbl_store: "Magasin /nix :",
    lbl_sandbox: "Bac à sable des compilations :",
    lbl_desktop: "Applications au menu :",
    lbl_path: "PATH :",
    val_absent: "absent",
    val_active: "actif",
    val_inactive: "inactif",
    val_unknown: "inconnu",
    val_yes: "oui",
    val_no: "non",
    mode_daemon: "multi-utilisateur (démon)",
    mode_single: "mono-utilisateur",
    hint_nix: "L'installation et la désinstallation de Nix demandent le mot de passe administrateur \
(pkexec).\nLes paquets, eux, s'installent sans root dans votre profil.",
    hint_store: "Recommandé : placer /nix sur un sous-volume btrfs dédié, exclu des instantanés\n\
(le magasin est volumineux et le restaurer ne sert à rien).",
    hint_path: "Pour que la version Nix d'un outil l'emporte, ~/.nix-profile/bin doit précéder \
/usr/bin dans le PATH\n(bouton « Intégrer les applications au menu » de l'onglet Maintenance).",
    hint_discover: "Ces paquets n'apparaissent ni dans Discover ni dans eopkg ;\n\
leurs mises à jour passent par Nix.",
    btn_refresh_state: "Actualiser l'état",
    btn_install_nix: "Installer Nix",
    btn_uninstall_nix: "Désinstaller Nix",

    lbl_search: "Rechercher dans nixpkgs (nom ou description)",
    btn_search: "Rechercher",
    col_name: "Paquet",
    col_version: "Version",
    col_desc: "Description",
    col_source: "Source",
    btn_install_sel: "Installer la sélection",
    lbl_flake: "Installer un flake ou un attribut (ex. nixpkgs#hello, github:utilisateur/dépôt)",
    btn_install_flake: "Installer",
    lbl_installed: "Paquets installés (profil utilisateur)",
    btn_refresh_profile: "Actualiser",
    btn_remove_sel: "Retirer la sélection",

    lbl_maintenance: "Mises à jour et nettoyage du profil utilisateur.",
    btn_upgrade: "Mettre à jour tous les paquets",
    btn_gc: "Nettoyer le magasin (nix-collect-garbage -d)",
    btn_desktop: "Intégrer les applications au menu",
    btn_sandbox: "Autoriser les compilations sans bac à sable",

    confirm_title: "Confirmation",
    confirm_uninstall: "Désinstaller Nix ?\n\n\
TOUS les paquets installés par Nix seront définitivement perdus (le magasin \
/nix est supprimé) : eopkg ne peut pas les restaurer. Le démon, les \
utilisateurs nixbld et les fichiers d'environnement sont retirés.\n\n\
Continuer ?",
    confirm_gc: "Nettoyer le magasin Nix ?\n\n\
Les anciennes générations du profil et tous les paquets qui n'en dépendent \
plus seront supprimés définitivement.",
    confirm_sandbox: "Désactiver le bac à sable des compilations ?\n\n\
Utile quand les namespaces utilisateur sont restreints par le noyau. Le cache \
binaire reste utilisable ; en revanche, les compilations locales ne sont plus \
isolées. Écrit « sandbox = false » dans /etc/nix/nix.conf et redémarre \
nix-daemon.",
    status_ready: "Prêt.",
    status_busy: "Opération en cours…",
    msg_no_nix: "Nix n'est pas installé : installez-le depuis l'onglet Nix.",
    msg_no_selection: "Sélectionnez d'abord une ligne.",
    msg_bad_flake: "Référence invalide (espaces et caractères de shell \
interdits).",
    msg_empty_term: "Saisissez un terme à rechercher.",
    search_hint_first: "Le premier appel télécharge nixpkgs (plusieurs \
minutes) ; les suivants sont rapides.",
    search_done: "{} résultat(s).",
    state_done: "État actualisé.",
    store_line: "{} dans /nix — système : {} / {} ({} libres)",
    store_only: "{} dans /nix",
    desktop_line: "{} application(s) intégrée(s) au menu.",
    profile_line: "{} paquet(s) dans le profil.",
    journal_placeholder: "Journal des opérations…",
    msg_already_installed: "Nix est déjà installé.",
    sandbox_ok: "actif (namespaces utilisateur disponibles)",
    sandbox_disabled: "sandbox = false",
    userns_missing: "namespaces utilisateur restreints",

    lbl_subvol: "Sous-volume /nix :",
    subvol_yes: "sous-volume btrfs dédié (exclu des instantanés)",
    subvol_no: "répertoire simple (inclus dans les instantanés de /)",
    subvol_na: "système de fichiers non btrfs",
    btn_subvol_create: "Créer le sous-volume /nix",
    btn_subvol_delete: "Supprimer le sous-volume /nix",
    confirm_subvol_delete: "Supprimer le sous-volume btrfs /nix ?\n\n\
Tout son contenu est détruit : les paquets Nix qu'il contient (le magasin) \
seraient perdus. À ne faire que sur un sous-volume vide ou après avoir \
désinstallé Nix.",
    btn_unfree_on: "Autoriser les paquets non libres",
    btn_unfree_off: "Interdire les paquets non libres",
    msg_no_btrfs: "Cette racine n'est pas sur btrfs : aucun sous-volume à créer.",
    msg_subvol_exists: "/nix est déjà un sous-volume btrfs.",
    msg_subvol_missing: "/nix n'est pas un sous-volume btrfs.",
    hint_subvol: "Un sous-volume btrfs dédié pour /nix sort le magasin des \
instantanés de / :\nc'est la recommandation par défaut. À créer avant la \
première installation de Nix.",
    hint_unfree: "Pour les paquets sous licence non libre (discord, steam…) :\n\
écrit ~/.config/nixpkgs/config.nix et passe NIXPKGS_ALLOW_UNFREE=1 aux \
installations.",
};

static EN: Texts = Texts {
    app_title: "Nix Manager",
    tab_nix: "Nix",
    tab_packages: "Packages",
    tab_maintenance: "Maintenance",

    menu_file: "File",
    menu_quit: "Quit",
    menu_refresh: "Refresh state",
    menu_help: "Help",
    menu_about: "About",
    about_title: "About Nix Manager",
    about_body: "Nix Manager — manage Nix and its packages on Solus.\n\n\
Installs and removes Nix (multi-user daemon mode), searches nixpkgs, installs \
packages or flakes into the USER profile (no root), upgrades and removes them.\n\n\
Packages installed by Nix appear neither in Discover nor in eopkg: updates go \
through Nix, never through the distribution package manager.\n\n\
License GPL-3.0-or-later.",

    lbl_nix: "Nix:",
    lbl_daemon: "nix-daemon:",
    lbl_mode: "Install mode:",
    lbl_profile: "User profile:",
    lbl_store: "/nix store:",
    lbl_sandbox: "Build sandbox:",
    lbl_desktop: "Menu entries:",
    lbl_path: "PATH:",
    val_absent: "absent",
    val_active: "active",
    val_inactive: "inactive",
    val_unknown: "unknown",
    val_yes: "yes",
    val_no: "no",
    mode_daemon: "multi-user (daemon)",
    mode_single: "single-user",
    hint_nix: "Installing and removing Nix requires the administrator password \
(pkexec).\nPackages are installed without root into your own profile.",
    hint_store: "Recommended: put /nix on a dedicated btrfs subvolume excluded \
from snapshots\n(the store is large and restoring it is useless).",
    hint_path: "So that Nix's copy of a tool wins, ~/.nix-profile/bin must come \
before /usr/bin in PATH\n(see \"Integrate menu entries\" in the Maintenance tab).",
    hint_discover: "These packages appear neither in Discover nor in eopkg;\n\
their updates go through Nix.",
    btn_refresh_state: "Refresh state",
    btn_install_nix: "Install Nix",
    btn_uninstall_nix: "Uninstall Nix",

    lbl_search: "Search nixpkgs (name or description)",
    btn_search: "Search",
    col_name: "Package",
    col_version: "Version",
    col_desc: "Description",
    col_source: "Source",
    btn_install_sel: "Install selection",
    lbl_flake: "Install a flake or attribute (e.g. nixpkgs#hello, github:user/repo)",
    btn_install_flake: "Install",
    lbl_installed: "Installed packages (user profile)",
    btn_refresh_profile: "Refresh",
    btn_remove_sel: "Remove selection",

    lbl_maintenance: "Upgrades and cleanup of the user profile.",
    btn_upgrade: "Upgrade all packages",
    btn_gc: "Clean the store (nix-collect-garbage -d)",
    btn_desktop: "Integrate menu entries",
    btn_sandbox: "Allow builds without sandbox",

    confirm_title: "Confirmation",
    confirm_uninstall: "Uninstall Nix?\n\n\
EVERY package installed by Nix will be lost for good (the /nix store is \
deleted): eopkg cannot restore them. The daemon, the nixbld users and the \
environment files are removed too.\n\n\
Continue?",
    confirm_gc: "Clean the Nix store?\n\n\
Old profile generations and every package no longer reachable from them will \
be deleted permanently.",
    confirm_sandbox: "Disable the build sandbox?\n\n\
Useful when the kernel restricts user namespaces. The binary cache stays \
usable, but local builds are no longer isolated. Writes \"sandbox = false\" to \
/etc/nix/nix.conf and restarts nix-daemon.",
    status_ready: "Ready.",
    status_busy: "Work in progress…",
    msg_no_nix: "Nix is not installed: install it from the Nix tab.",
    msg_no_selection: "Select a row first.",
    msg_bad_flake: "Invalid reference (no spaces or shell characters).",
    msg_empty_term: "Type something to search for.",
    search_hint_first: "The first call downloads nixpkgs (several minutes); \
later searches are fast.",
    search_done: "{} result(s).",
    state_done: "State refreshed.",
    store_line: "{} in /nix — filesystem: {} / {} ({} free)",
    store_only: "{} in /nix",
    desktop_line: "{} application(s) integrated into the menu.",
    profile_line: "{} package(s) in the profile.",
    journal_placeholder: "Operation log…",
    msg_already_installed: "Nix is already installed.",
    sandbox_ok: "active (user namespaces available)",
    sandbox_disabled: "sandbox = false",
    userns_missing: "user namespaces restricted",

    lbl_subvol: "/nix subvolume:",
    subvol_yes: "dedicated btrfs subvolume (excluded from snapshots)",
    subvol_no: "plain directory (included in / snapshots)",
    subvol_na: "not a btrfs filesystem",
    btn_subvol_create: "Create the /nix subvolume",
    btn_subvol_delete: "Delete the /nix subvolume",
    confirm_subvol_delete: "Delete the /nix btrfs subvolume?\n\n\
All of its content is destroyed: any Nix package (the store) it holds would be \
lost. Only do this on an empty subvolume or after uninstalling Nix.",
    btn_unfree_on: "Allow unfree packages",
    btn_unfree_off: "Disallow unfree packages",
    msg_no_btrfs: "This root is not on btrfs: there is no subvolume to create.",
    msg_subvol_exists: "/nix is already a btrfs subvolume.",
    msg_subvol_missing: "/nix is not a btrfs subvolume.",
    hint_subvol: "A dedicated btrfs subvolume keeps the store out of / \
snapshots:\nthat is the recommended setup. Create it before installing Nix.",
    hint_unfree: "For packages under an unfree licence (discord, steam…):\n\
writes ~/.config/nixpkgs/config.nix and passes NIXPKGS_ALLOW_UNFREE=1 to \
installs.",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_fr() {
        assert_eq!(from_locale("fr_FR.UTF-8"), Lang::Fr);
        assert_eq!(from_locale("fr"), Lang::Fr);
    }

    #[test]
    fn locale_en_et_repli() {
        assert_eq!(from_locale("en_US.UTF-8"), Lang::En);
        assert_eq!(from_locale("C"), Lang::En);
        assert_eq!(from_locale("de_DE"), Lang::En);
    }

    #[test]
    fn langue_explicite() {
        assert_eq!(parse_lang("fr"), Lang::Fr);
        assert_eq!(parse_lang("FR"), Lang::Fr);
        assert_eq!(parse_lang("en_GB"), Lang::En);
    }

    #[test]
    fn fill_remplace_dans_l_ordre() {
        assert_eq!(fill("{} / {} ({})", &["1", "2", "3"]), "1 / 2 (3)");
        assert_eq!(fill("aucun trou", &["x"]), "aucun trou");
        assert_eq!(fill("{} seul", &[]), "{} seul");
    }
}
