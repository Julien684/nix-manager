//! Nix Manager — interface Qt6 (via `qtrs`), binaire unique.
//!
//! Trois onglets (Nix / Paquets / Maintenance), un journal verbeux en direct et
//! une pompe `QTimer` qui vide le canal des workers dans le thread UI : aucune
//! opération ne bloque la fenêtre.
//!
//! Modèle d'ownership qtrs (pattern validé sur `eopkg-converter` /
//! `ollama-manager-qt`) :
//!  - les widgets sont créés avec un objet-nom et retrouvés dans les callbacks
//!    et dans la pompe via `ffi::QWidget_find*` à partir du pointeur du widget
//!    central (jamais de `Rc` fort capturé dans une closure de signal) ;
//!  - les layouts sont des variables locales de `main` : ils vivent jusqu'à la
//!    fin de `app.exec()` (un layout dropé supprimerait son C++) ;
//!  - le `QTimer` de la pompe est construit (`.build()`) et conservé jusqu'à la
//!    fin de `exec()`.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;

use cxx::let_cxx_string;
use qtrs::prelude::*;
use qtrs::{ffi, Application, MessageBox, TabWidget, Timer};

mod i18n;
mod job;
mod nix;

use i18n::{fill, Lang, Texts};
use job::JobKind;
use nix::Mode;

/// Boutons désactivés pendant une opération (objet-noms).
const BUSY_BUTTONS: [&str; 15] = [
    "btn_refresh",
    "btn_install_nix",
    "btn_uninstall_nix",
    "btn_search",
    "btn_install_sel",
    "btn_install_flake",
    "btn_profile_refresh",
    "btn_remove_sel",
    "btn_upgrade",
    "btn_gc",
    "btn_desktop",
    "btn_sandbox",
    "btn_subvol_create",
    "btn_subvol_delete",
    "btn_unfree",
];

/// Valeurs des boutons Qt (QMessageBox::StandardButton).
pub const BTN_YES: i32 = 0x0000_4000;
pub const BTN_CANCEL: i32 = 0x0040_0000;

// ------------------------------------------------------------------
// Dialogues
// ------------------------------------------------------------------

/// Confirmation modale (Oui / Annuler), sans parent : un `exec()` tenant un
/// `RefCell` emprunté ferait échouer l'action en silence.
fn confirm(title: &str, text: &str) -> bool {
    let mb = MessageBox::new()
        .icon(3) // Question
        .text(text)
        .window_title(title)
        .standard_buttons(BTN_YES | BTN_CANCEL)
        .default_button(BTN_YES)
        .build();
    mb.exec() == BTN_YES
}

fn about(title: &str, body: &str) {
    MessageBox::about(None, title, body);
}

/// Ajoute une ligne « libellé : valeur » dans une grille (2 colonnes).
fn add_pair(grid: &mut GridLayout, page: &Widget, row: i32, label: &str, name: &str) {
    let l = Label::new(label).parent(page).build();
    grid.add(l, row, 0, 1, 1);
    let v = Label::new("—").parent(page).build();
    v.set_object_name(name);
    grid.add(v, row, 1, 1, 1);
}

// ------------------------------------------------------------------
// Accès aux widgets par objet-nom
// ------------------------------------------------------------------

fn set_label(central: *mut ffi::QWidget, name: &str, text: &str) {
    unsafe {
        let_cxx_string!(n = name);
        let lb = ffi::QWidget_findLabel(central, &n);
        if lb.is_null() {
            return;
        }
        let_cxx_string!(s = text);
        ffi::QLabel_setText(lb, &s);
    }
}

fn append_log(central: *mut ffi::QWidget, text: &str) {
    unsafe {
        let_cxx_string!(n = "log");
        let w = ffi::QWidget_findPlainTextEdit(central, &n);
        if w.is_null() {
            return;
        }
        let_cxx_string!(s = text);
        ffi::QPlainTextEdit_appendPlainText(w, &s);
    }
}

fn clear_log(central: *mut ffi::QWidget) {
    unsafe {
        let_cxx_string!(n = "log");
        let w = ffi::QWidget_findPlainTextEdit(central, &n);
        if !w.is_null() {
            ffi::QPlainTextEdit_clear(w);
        }
    }
}

fn read_edit(central: *mut ffi::QWidget, name: &str) -> String {
    unsafe {
        let_cxx_string!(n = name);
        let e = ffi::QWidget_findLineEdit(central, &n);
        if e.is_null() {
            String::new()
        } else {
            ffi::QLineEdit_text(e)
        }
    }
}

fn table_row(central: *mut ffi::QWidget, name: &str) -> i32 {
    unsafe {
        let_cxx_string!(n = name);
        let t = ffi::QWidget_findTableWidget(central, &n);
        if t.is_null() {
            -1
        } else {
            ffi::QTableWidget_currentRow(t)
        }
    }
}

fn table_item(central: *mut ffi::QWidget, name: &str, row: i32, col: i32) -> String {
    unsafe {
        let_cxx_string!(n = name);
        let t = ffi::QWidget_findTableWidget(central, &n);
        if t.is_null() {
            String::new()
        } else {
            ffi::QTableWidget_itemText(t, row, col)
        }
    }
}

fn fill_table(central: *mut ffi::QWidget, name: &str, rows: &[(String, String, String)]) {
    unsafe {
        let_cxx_string!(n = name);
        let t = ffi::QWidget_findTableWidget(central, &n);
        if t.is_null() {
            return;
        }
        ffi::QTableWidget_setRowCount(t, rows.len() as i32);
        for (i, (a, b, c)) in rows.iter().enumerate() {
            for (col, text) in [(0, a), (1, b), (2, c)] {
                let_cxx_string!(s = text);
                ffi::QTableWidget_setItem(t, i as i32, col, &s);
            }
        }
    }
}

/// Active/désactive les boutons d'action et l'indicateur d'occupation.
fn set_busy(central: *mut ffi::QWidget, busy: bool, t: &Texts) {
    unsafe {
        for name in BUSY_BUTTONS {
            let_cxx_string!(n = name);
            let btn = ffi::QWidget_findPushButton(central, &n);
            if !btn.is_null() {
                ffi::QWidget_setEnabled(btn as *mut ffi::QWidget, !busy);
            }
        }
        let_cxx_string!(n = "bar");
        let bar = ffi::QWidget_findProgressBar(central, &n);
        if !bar.is_null() {
            if busy {
                // 0 → 0 : Qt affiche une barre « occupée » indéterminée.
                ffi::QProgressBar_setRange(bar, 0, 0);
            } else {
                ffi::QProgressBar_setRange(bar, 0, 100);
                ffi::QProgressBar_setValue(bar, 0);
            }
        }
        let_cxx_string!(ns = "status");
        let st = ffi::QWidget_findLabel(central, &ns);
        if !st.is_null() {
            let_cxx_string!(s = if busy { t.status_busy } else { t.status_ready });
            ffi::QLabel_setText(st, &s);
        }
    }
}

/// Renseigne les libellés de l'onglet « Nix » depuis un état détecté.
fn render_state(central: *mut ffi::QWidget, st: &nix::NixState, t: &Texts) {
    let nix = match &st.version {
        Some(v) => v.clone(),
        None => t.val_absent.to_string(),
    };
    set_label(central, "nix_state", &nix);

    let daemon = if st.daemon {
        t.val_active
    } else if st.mode == Mode::Absent {
        t.val_absent
    } else {
        t.val_inactive
    };
    set_label(central, "daemon_state", daemon);

    let mode = match st.mode {
        Mode::Daemon => t.mode_daemon,
        Mode::SingleUser => t.mode_single,
        Mode::Absent => t.val_absent,
        Mode::Unknown => t.val_unknown,
    };
    set_label(central, "mode_state", mode);

    let profile = if st.profile_exists {
        st.profile.clone()
    } else {
        format!("{} ({})", st.profile, t.val_absent)
    };
    set_label(central, "profile_state", &profile);

    let store = match (st.store_bytes, st.fs_used, st.fs_total) {
        (Some(bytes), Some(used), Some(total)) => fill(
            t.store_line,
            &[
                &nix::human(bytes),
                &nix::human(used),
                &nix::human(total),
                &nix::human(total.saturating_sub(used)),
            ],
        ),
        (Some(bytes), _, _) => fill(t.store_only, &[&nix::human(bytes)]),
        _ => t.val_unknown.to_string(),
    };
    set_label(central, "store_state", &store);
    unsafe {
        let_cxx_string!(n = "store_bar");
        let bar = ffi::QWidget_findProgressBar(central, &n);
        if !bar.is_null() {
            ffi::QProgressBar_setRange(bar, 0, 100);
            let pct = match (st.fs_used, st.fs_total) {
                (Some(used), Some(total)) if total > 0 => (used * 100 / total) as i32,
                _ => 0,
            };
            ffi::QProgressBar_setValue(bar, pct);
        }
    }

    let sandbox = if st.mode == Mode::Absent {
        t.val_unknown
    } else if st.sandbox_off {
        t.sandbox_disabled
    } else if st.userns {
        t.sandbox_ok
    } else {
        t.userns_missing
    };
    set_label(central, "sandbox_state", sandbox);

    let desktop = if st.desktop_count == 0 {
        t.val_no.to_string()
    } else {
        fill(t.desktop_line, &[&st.desktop_count.to_string()])
    };
    set_label(central, "desktop_state", &desktop);
    set_label(
        central,
        "path_state",
        if st.path_ok { t.val_yes } else { t.val_no },
    );

    let subvol = if !st.btrfs {
        t.subvol_na
    } else if st.nix_subvol {
        t.subvol_yes
    } else {
        t.subvol_no
    };
    set_label(central, "subvol_state", subvol);

    // Le libellé du bouton décrit l'action disponible dans l'état courant.
    unsafe {
        let_cxx_string!(n = "btn_unfree");
        let btn = ffi::QWidget_findPushButton(central, &n);
        if !btn.is_null() {
            let_cxx_string!(s = if st.unfree { t.btn_unfree_off } else { t.btn_unfree_on });
            ffi::QPushButton_setText(btn, &s);
        }
    }
}

// ------------------------------------------------------------------
// Icône
// ------------------------------------------------------------------

fn resolve_icon() -> Option<String> {
    for candidate in [
        "/usr/share/pixmaps/nix-manager.png",
        "/usr/share/icons/hicolor/128x128/apps/nix-manager.png",
    ] {
        if Path::new(candidate).exists() {
            return Some(candidate.to_string());
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    for rel in ["assets/nix-manager.svg", "assets/nix-manager-128.png"] {
        let p = dir.join(rel);
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
}

// ------------------------------------------------------------------
// Mode texte (tests / diagnostic)
// ------------------------------------------------------------------

fn print_status() {
    let st = nix::detect();
    println!("nix_bin={}", st.nix_bin.clone().unwrap_or_default());
    println!("version={}", st.version.clone().unwrap_or_default());
    println!("daemon={}", st.daemon);
    println!("mode={:?}", st.mode);
    println!("store={}", st.store);
    println!(
        "store_bytes={}",
        st.store_bytes.map(|v| v.to_string()).unwrap_or_default()
    );
    println!(
        "fs_used={}",
        st.fs_used.map(|v| v.to_string()).unwrap_or_default()
    );
    println!(
        "fs_total={}",
        st.fs_total.map(|v| v.to_string()).unwrap_or_default()
    );
    println!("profile={}", st.profile);
    println!("profile_exists={}", st.profile_exists);
    println!("userns={}", st.userns);
    println!("sandbox_off={}", st.sandbox_off);
    println!("installer={}", st.installer);
    println!("desktop_count={}", st.desktop_count);
    println!("xdg_conf={}", st.xdg_conf);
    println!("path_ok={}", st.path_ok);
    println!("pkexec={}", st.pkexec);
    println!("btrfs={}", st.btrfs);
    println!("btrfs_progs={}", st.btrfs_progs);
    println!("nix_subvol={}", st.nix_subvol);
    println!("unfree={}", st.unfree);
}

// ------------------------------------------------------------------
// Interface
// ------------------------------------------------------------------

fn main() {
    let lang = Lang::detect();
    let t: &'static Texts = lang.texts();

    if std::env::args().any(|a| a == "--status") {
        print_status();
        return;
    }

    let app = Application::new();
    if let Some(icon) = resolve_icon() {
        app.set_icon(&icon);
    }
    // Associe la fenêtre au .desktop (icône dans le sélecteur de tâches).
    app.set_desktop_file_name("nix-manager");

    let window = MainWindow::new()
        .window_title(t.app_title)
        .size(1120, 800)
        .build();

    // ---- barre de menus ----------------------------------------------------
    let file_menu = Menu::new(t.menu_file)
        .action(t.menu_refresh, || {})
        .action(t.menu_quit, || std::process::exit(0))
        .build();
    let help_menu = Menu::new(t.menu_help)
        .action(t.menu_about, || about(t.about_title, t.about_body))
        .build();
    let menubar = MenuBar::new().add_menu(file_menu).add_menu(help_menu).build();
    window.set_menu_bar(&menubar);
    // `set_menu_bar` confie la barre à Qt : on oublie le wrapper (double delete).
    std::mem::forget(menubar);

    // ---- widget central ----------------------------------------------------
    let mut central = Widget::new().build();
    // Sans ce drapeau, le wrapper Rust détruit le C++ alors que QMainWindow
    // garde le pointeur → segfault au premier paint.
    central.set_has_parent();
    let central_ptr = central.widget_ptr() as *mut ffi::QWidget;
    let mut root = VBoxLayout::with_parent(&central);
    root.set_contents_margins(12, 12, 12, 12);
    root.set_spacing(8);

    let (tx, rx) = mpsc::channel::<job::Msg>();
    // Nix présent ? (relu à chaque clic, sans I/O lourde)
    let nix_present = Rc::new(Cell::new(false));

    // ==================================================================
    // Onglet 1 — état de Nix
    // ==================================================================
    let nix_page = Widget::new().build();
    let mut nix_grid = GridLayout::with_parent(&nix_page);

    let mut row = 0;
    for (label, name) in [
        (t.lbl_nix, "nix_state"),
        (t.lbl_daemon, "daemon_state"),
        (t.lbl_mode, "mode_state"),
        (t.lbl_profile, "profile_state"),
        (t.lbl_store, "store_state"),
    ] {
        add_pair(&mut nix_grid, &nix_page, row, label, name);
        row += 1;
    }

    let store_bar = ProgressBar::new().parent(&nix_page).range(0, 100).value(0).build();
    store_bar.set_object_name("store_bar");
    nix_grid.add(store_bar, row, 1, 1, 1);
    row += 1;

    for (label, name) in [
        (t.lbl_sandbox, "sandbox_state"),
        (t.lbl_subvol, "subvol_state"),
        (t.lbl_desktop, "desktop_state"),
        (t.lbl_path, "path_state"),
    ] {
        add_pair(&mut nix_grid, &nix_page, row, label, name);
        row += 1;
    }

    // Boutons de l'onglet Nix.
    let nix_actions = Frame::new().parent(&nix_page).build();
    let mut nix_actions_l = HBoxLayout::with_parent(&nix_actions);
    nix_actions_l.set_spacing(8);

    let btn_refresh = PushButton::new(t.btn_refresh_state)
        .parent(&nix_actions)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                clear_log(cp);
                set_busy(cp, true, t);
                job::spawn_state(tx.clone());
            }
        })
        .build();
    btn_refresh.set_object_name("btn_refresh");
    nix_actions_l.add(btn_refresh);

    let btn_install_nix = PushButton::new(t.btn_install_nix)
        .parent(&nix_actions)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            let present = Rc::clone(&nix_present);
            move || {
                // Un Nix complet (binaire + démon) n'a rien à installer.
                if present.get() && nix::daemon_active() {
                    set_label(cp, "status", t.msg_already_installed);
                    return;
                }
                // Restes d'une installation précédente : l'installeur officiel
                // refuse de démarrer tant qu'ils sont là (« … already exists »).
                let blockers = nix::install_blockers();
                let mut clean_first = false;
                if blockers.any() {
                    let list = blockers.describe().join("\n");
                    let text = fill(t.confirm_install_clean, &[&list]);
                    if !confirm(t.confirm_title, &text) {
                        return;
                    }
                    clean_first = true;
                }
                clear_log(cp);
                append_log(cp, "== Installation de Nix (mode démon) ==");
                set_busy(cp, true, t);
                job::spawn_install_nix(tx.clone(), clean_first);
            }
        })
        .build();
    btn_install_nix.set_object_name("btn_install_nix");
    nix_actions_l.add(btn_install_nix);

    let btn_uninstall_nix = PushButton::new(t.btn_uninstall_nix)
        .parent(&nix_actions)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                if !confirm(t.confirm_title, t.confirm_uninstall) {
                    return;
                }
                clear_log(cp);
                append_log(cp, "== Désinstallation de Nix ==");
                set_busy(cp, true, t);
                job::spawn_uninstall_nix(tx.clone());
            }
        })
        .build();
    btn_uninstall_nix.set_object_name("btn_uninstall_nix");
    nix_actions_l.add(btn_uninstall_nix);
    nix_grid.add(nix_actions, row, 0, 1, 2);
    row += 1;

    // Sous-volume btrfs : création / suppression (root via pkexec).
    let subvol_actions = Frame::new().parent(&nix_page).build();
    let mut subvol_l = HBoxLayout::with_parent(&subvol_actions);
    subvol_l.set_spacing(8);

    let btn_subvol_create = PushButton::new(t.btn_subvol_create)
        .parent(&subvol_actions)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                if nix::fs_type("/") != "btrfs" {
                    set_label(cp, "status", t.msg_no_btrfs);
                    return;
                }
                match nix::nix_path_state() {
                    nix::NixPath::Subvolume => {
                        set_label(cp, "status", t.msg_subvol_exists);
                        return;
                    }
                    nix::NixPath::NonEmptyDir => {
                        set_label(cp, "status", t.msg_subvol_blocked_full);
                        return;
                    }
                    nix::NixPath::Mounted => {
                        set_label(cp, "status", t.msg_subvol_blocked_mount);
                        return;
                    }
                    nix::NixPath::Absent | nix::NixPath::EmptyDir => {}
                }
                clear_log(cp);
                set_busy(cp, true, t);
                job::spawn_subvol(tx.clone(), true);
            }
        })
        .build();
    btn_subvol_create.set_object_name("btn_subvol_create");
    subvol_l.add(btn_subvol_create);

    let btn_subvol_delete = PushButton::new(t.btn_subvol_delete)
        .parent(&subvol_actions)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                if !nix::is_subvolume("/nix") {
                    set_label(cp, "status", t.msg_subvol_missing);
                    return;
                }
                if !confirm(t.confirm_title, t.confirm_subvol_delete) {
                    return;
                }
                clear_log(cp);
                set_busy(cp, true, t);
                job::spawn_subvol(tx.clone(), false);
            }
        })
        .build();
    btn_subvol_delete.set_object_name("btn_subvol_delete");
    subvol_l.add(btn_subvol_delete);
    nix_grid.add(subvol_actions, row, 0, 1, 2);
    row += 1;

    for hint in [t.hint_nix, t.hint_subvol, t.hint_store, t.hint_path, t.hint_discover] {
        let l = Label::new(hint).parent(&nix_page).build();
        nix_grid.add(l, row, 0, 1, 2);
        row += 1;
    }

    // ==================================================================
    // Onglet 2 — paquets (recherche, flake, profil)
    // ==================================================================
    let pkg_page = Widget::new().build();
    let mut pkg_l = VBoxLayout::with_parent(&pkg_page);
    pkg_l.set_spacing(6);

    let search_lbl = Label::new(t.lbl_search).parent(&pkg_page).build();
    pkg_l.add(search_lbl);
    let search_row = Frame::new().parent(&pkg_page).build();
    let mut search_l = HBoxLayout::with_parent(&search_row);
    search_l.set_spacing(8);
    // Champ créé VIDE : `LineEdit::new("…")` écrirait du texte réel, pas un
    // placeholder.
    let search_edit = LineEdit::new("").parent(&search_row).build();
    search_edit.set_object_name("search_edit");
    search_l.add(search_edit);
    let btn_search = PushButton::new(t.btn_search)
        .parent(&search_row)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                let term = read_edit(cp, "search_edit").trim().to_string();
                if term.is_empty() {
                    set_label(cp, "status", t.msg_empty_term);
                    return;
                }
                if !nix::nix_bin().is_some() {
                    set_label(cp, "status", t.msg_no_nix);
                    return;
                }
                clear_log(cp);
                append_log(cp, t.search_hint_first);
                set_busy(cp, true, t);
                job::spawn_search(tx.clone(), term);
            }
        })
        .build();
    btn_search.set_object_name("btn_search");
    search_l.add(btn_search);
    pkg_l.add(search_row);

    let search_table = TableWidget::new(0, 3).parent(&pkg_page).build();
    search_table.set_object_name("search_table");
    search_table.set_horizontal_header_labels(&[t.col_name, t.col_version, t.col_desc]);
    search_table.set_column_width(0, 300);
    search_table.set_column_width(1, 110);
    search_table.set_column_width(2, 640);
    pkg_l.add(search_table);

    let install_row = Frame::new().parent(&pkg_page).build();
    let mut install_l = HBoxLayout::with_parent(&install_row);
    install_l.set_spacing(8);
    let btn_install_sel = PushButton::new(t.btn_install_sel)
        .parent(&install_row)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                let r = table_row(cp, "search_table");
                if r < 0 {
                    set_label(cp, "status", t.msg_no_selection);
                    return;
                }
                let attr = table_item(cp, "search_table", r, 0);
                if attr.is_empty() {
                    set_label(cp, "status", t.msg_no_selection);
                    return;
                }
                if !nix::nix_bin().is_some() {
                    set_label(cp, "status", t.msg_no_nix);
                    return;
                }
                set_busy(cp, true, t);
                append_log(cp, &format!("Installation de {attr}…"));
                job::spawn_pkg_install(tx.clone(), nix::install_target(&attr));
            }
        })
        .build();
    btn_install_sel.set_object_name("btn_install_sel");
    install_l.add(btn_install_sel);
    pkg_l.add(install_row);

    let flake_lbl = Label::new(t.lbl_flake).parent(&pkg_page).build();
    pkg_l.add(flake_lbl);
    let flake_row = Frame::new().parent(&pkg_page).build();
    let mut flake_l = HBoxLayout::with_parent(&flake_row);
    flake_l.set_spacing(8);
    let flake_edit = LineEdit::new("").parent(&flake_row).build();
    flake_edit.set_object_name("flake_edit");
    flake_l.add(flake_edit);
    let btn_install_flake = PushButton::new(t.btn_install_flake)
        .parent(&flake_row)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                let raw = read_edit(cp, "flake_edit").trim().to_string();
                if !nix::flake_ref_ok(&raw) {
                    set_label(cp, "status", t.msg_bad_flake);
                    return;
                }
                if !nix::nix_bin().is_some() {
                    set_label(cp, "status", t.msg_no_nix);
                    return;
                }
                let target = nix::install_target(&raw);
                set_busy(cp, true, t);
                append_log(cp, &format!("Installation de {target}…"));
                job::spawn_pkg_install(tx.clone(), target);
            }
        })
        .build();
    btn_install_flake.set_object_name("btn_install_flake");
    flake_l.add(btn_install_flake);
    pkg_l.add(flake_row);

    let installed_lbl = Label::new(t.lbl_installed).parent(&pkg_page).build();
    pkg_l.add(installed_lbl);

    let profile_table = TableWidget::new(0, 2).parent(&pkg_page).build();
    profile_table.set_object_name("profile_table");
    profile_table.set_horizontal_header_labels(&[t.col_name, t.col_source]);
    profile_table.set_column_width(0, 300);
    profile_table.set_column_width(1, 750);
    pkg_l.add(profile_table);

    let profile_row = Frame::new().parent(&pkg_page).build();
    let mut profile_l = HBoxLayout::with_parent(&profile_row);
    profile_l.set_spacing(8);
    let btn_profile_refresh = PushButton::new(t.btn_refresh_profile)
        .parent(&profile_row)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                if !nix::nix_bin().is_some() {
                    set_label(cp, "status", t.msg_no_nix);
                    return;
                }
                set_busy(cp, true, t);
                job::spawn_profile(tx.clone());
            }
        })
        .build();
    btn_profile_refresh.set_object_name("btn_profile_refresh");
    profile_l.add(btn_profile_refresh);

    let btn_remove_sel = PushButton::new(t.btn_remove_sel)
        .parent(&profile_row)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                let r = table_row(cp, "profile_table");
                if r < 0 {
                    set_label(cp, "status", t.msg_no_selection);
                    return;
                }
                let name = table_item(cp, "profile_table", r, 0);
                if name.is_empty() {
                    set_label(cp, "status", t.msg_no_selection);
                    return;
                }
                set_busy(cp, true, t);
                append_log(cp, &format!("Retrait de {name}…"));
                job::spawn_pkg_remove(tx.clone(), name);
            }
        })
        .build();
    btn_remove_sel.set_object_name("btn_remove_sel");
    profile_l.add(btn_remove_sel);
    pkg_l.add(profile_row);

    // ==================================================================
    // Onglet 3 — maintenance
    // ==================================================================
    let maint_page = Widget::new().build();
    let mut maint_l = VBoxLayout::with_parent(&maint_page);
    maint_l.set_spacing(10);

    let maint_lbl = Label::new(t.lbl_maintenance).parent(&maint_page).build();
    maint_l.add(maint_lbl);

    let maint_row = Frame::new().parent(&maint_page).build();
    let mut maint_r = HBoxLayout::with_parent(&maint_row);
    maint_r.set_spacing(8);

    let btn_upgrade = PushButton::new(t.btn_upgrade)
        .parent(&maint_row)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                if !nix::nix_bin().is_some() {
                    set_label(cp, "status", t.msg_no_nix);
                    return;
                }
                clear_log(cp);
                set_busy(cp, true, t);
                job::spawn_upgrade(tx.clone());
            }
        })
        .build();
    btn_upgrade.set_object_name("btn_upgrade");
    maint_r.add(btn_upgrade);

    let btn_gc = PushButton::new(t.btn_gc)
        .parent(&maint_row)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                if !nix::nix_bin().is_some() {
                    set_label(cp, "status", t.msg_no_nix);
                    return;
                }
                if !confirm(t.confirm_title, t.confirm_gc) {
                    return;
                }
                clear_log(cp);
                set_busy(cp, true, t);
                job::spawn_gc(tx.clone());
            }
        })
        .build();
    btn_gc.set_object_name("btn_gc");
    maint_r.add(btn_gc);

    let btn_desktop = PushButton::new(t.btn_desktop)
        .parent(&maint_row)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                if !nix::nix_bin().is_some() {
                    set_label(cp, "status", t.msg_no_nix);
                    return;
                }
                clear_log(cp);
                set_busy(cp, true, t);
                job::spawn_desktop(tx.clone());
            }
        })
        .build();
    btn_desktop.set_object_name("btn_desktop");
    maint_r.add(btn_desktop);

    let btn_sandbox = PushButton::new(t.btn_sandbox)
        .parent(&maint_row)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                if !confirm(t.confirm_title, t.confirm_sandbox) {
                    return;
                }
                clear_log(cp);
                set_busy(cp, true, t);
                job::spawn_sandbox(tx.clone());
            }
        })
        .build();
    btn_sandbox.set_object_name("btn_sandbox");
    maint_r.add(btn_sandbox);
    maint_l.add(maint_row);

    // Paquets non libres : bascule ~/.config/nixpkgs/config.nix.
    let unfree_row = Frame::new().parent(&maint_page).build();
    let mut unfree_l = HBoxLayout::with_parent(&unfree_row);
    unfree_l.set_spacing(8);
    let btn_unfree = PushButton::new(t.btn_unfree_on)
        .parent(&unfree_row)
        .on_clicked({
            let tx = tx.clone();
            let cp = central_ptr;
            move || {
                let enable = !nix::unfree_enabled();
                clear_log(cp);
                set_busy(cp, true, t);
                job::spawn_unfree(tx.clone(), enable);
            }
        })
        .build();
    btn_unfree.set_object_name("btn_unfree");
    unfree_l.add(btn_unfree);
    let unfree_hint = Label::new(t.hint_unfree).parent(&unfree_row).build();
    unfree_l.add(unfree_hint);
    maint_l.add(unfree_row);

    let maint_hint = Label::new(t.hint_discover).parent(&maint_page).build();
    maint_l.add(maint_hint);

    // ==================================================================
    // Onglets + pied de page
    // ==================================================================
    let mut tabs = TabWidget::new().build();
    tabs.add_tab(Box::new(nix_page), t.tab_nix);
    tabs.add_tab(Box::new(pkg_page), t.tab_packages);
    tabs.add_tab(Box::new(maint_page), t.tab_maintenance);
    root.add(tabs);

    let footer = Frame::new().parent(&central).build();
    let mut footer_l = HBoxLayout::with_parent(&footer);
    footer_l.set_spacing(8);
    let bar = ProgressBar::new().parent(&footer).range(0, 100).value(0).build();
    bar.set_object_name("bar");
    footer_l.add(bar);
    let status = Label::new(t.status_ready).parent(&footer).build();
    status.set_object_name("status");
    footer_l.add(status);
    root.add(footer);

    let log = PlainTextEdit::new().parent(&central).build();
    log.set_object_name("log");
    log.set_placeholder_text(t.journal_placeholder);
    log.set_read_only(true);
    unsafe {
        ffi::QWidget_setFixedHeight(log.widget_ptr() as *mut ffi::QWidget, 160);
    }
    root.add(log);

    window.set_central_widget(&central);
    window.show();

    // ---- pompe : canal des workers → widgets (thread UI) -------------------
    let tx_bg = tx.clone();
    let _timer = Timer::new(150)
        .on_timeout({
            let present = Rc::clone(&nix_present);
            move || {
                let cp = central_ptr;
                while let Ok(msg) = rx.try_recv() {
                    match msg {
                        job::Msg::Log(line) => append_log(cp, &line),
                        job::Msg::State(st) => {
                            present.set(st.version.is_some());
                            render_state(cp, &st, t);
                        }
                        job::Msg::Search(hits) => {
                            let rows: Vec<(String, String, String)> = hits
                                .iter()
                                .map(|h| (h.attr.clone(), h.version.clone(), h.description.clone()))
                                .collect();
                            fill_table(cp, "search_table", &rows);
                        }
                        job::Msg::Profile(entries) => {
                            let rows: Vec<(String, String, String)> = entries
                                .iter()
                                .map(|e| {
                                    let version = String::new();
                                    (e.name.clone(), version, e.source.clone())
                                })
                                .collect();
                            fill_table(cp, "profile_table", &rows);
                        }
                        job::Msg::Done { kind, ok, detail } => {
                            set_busy(cp, false, t);
                            let mark = if ok { "✔" } else { "✘" };
                            let text = match kind {
                                JobKind::Search if ok => fill(t.search_done, &[&detail]),
                                JobKind::State => t.state_done.to_string(),
                                JobKind::Profile if ok => fill(t.profile_line, &[&detail]),
                                _ => detail.clone(),
                            };
                            set_label(cp, "status", &format!("{mark} {text}"));
                            append_log(cp, &format!("{mark} {text}"));
                            match kind {
                                JobKind::InstallNix
                                | JobKind::UninstallNix
                                | JobKind::Desktop
                                | JobKind::Sandbox
                                | JobKind::Subvol
                                | JobKind::Unfree
                                | JobKind::Gc => job::spawn_state(tx_bg.clone()),
                                JobKind::InstallPkg
                                | JobKind::RemovePkg
                                | JobKind::Upgrade => {
                                    job::spawn_profile(tx_bg.clone());
                                    job::spawn_state(tx_bg.clone());
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        })
        .build();

    // État initial + profil, sans bloquer l'affichage.
    job::spawn_state(tx.clone());
    if nix::nix_bin().is_some() {
        job::spawn_profile(tx.clone());
    }

    // Hook de test : quitte proprement après N ms (smoke offscreen/Xvfb).
    if let Ok(ms) = std::env::var("NIX_MANAGER_SMOKE_MS") {
        if let Ok(ms) = ms.parse::<i32>() {
            Timer::single_shot(ms, || std::process::exit(0));
        }
    }

    app.exec();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boutons_dialogue_distincts() {
        assert_ne!(BTN_YES, BTN_CANCEL);
        assert_eq!(BTN_YES, 0x4000);
        assert_eq!(BTN_CANCEL, 0x0040_0000);
    }

    #[test]
    fn tous_les_boutons_ont_un_nom_unique() {
        let mut names = BUSY_BUTTONS.to_vec();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());
    }
}
