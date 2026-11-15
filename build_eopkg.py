#!/usr/bin/env python3
"""
build_eopkg.py — Construit un .eopkg pour nix-manager au format binaire Solus.

L'archive est un ZIP contenant :
  - metadata.xml
  - files.xml
  - install.tar.xz

Aucun script COMAR (paquet de fichiers purs). Dépendance runtime unique :
qt6-base-gui (nom Solus de QtGui/QtWidgets), au format PISI « texte nu »
<Dependency>qt6-base-gui</Dependency> — toute autre forme est rejetée par
l'analyseur d'eopkg 5.0.
"""
import hashlib
import io
import sys
import tarfile
import zipfile
from datetime import date
from pathlib import Path

# ──────────────────────────────────────────────────────────────────────────────
# Configuration
# ──────────────────────────────────────────────────────────────────────────────
SRC_DIR = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(".")
OUT_DIR = Path(sys.argv[2]) if len(sys.argv) > 2 else SRC_DIR

PACKAGE_NAME   = "nix-manager"
VERSION        = "0.1.0"
RELEASE        = 1
DIST_RELEASE   = 1
ARCH           = "x86_64"
DIST_NAME      = "Solus"
BUILD_HOST     = "solus-local"
HOME_PAGE      = "https://depoa.julien68.fr/Linuxon/nix-manager"
PACKAGER_NAME  = "Julien"
PACKAGER_EMAIL = "julien_solus@use.startmail.com"
PART_OF        = "system.utils"

# (dest_path, src_rel_path, mode)
FILE_LIST = [
    ("usr/bin/nix-manager", "target/release/nix-manager", 0o755),
    ("usr/share/applications/nix-manager.desktop", "nix-manager.desktop", 0o644),
    # Copie hors thème : le .desktop pointe ce chemin absolu, que les menus
    # (KDE/GTK) résolvent toujours, contrairement au seul nom de thème.
    ("usr/share/pixmaps/nix-manager.png", "icons/nix-manager-256.png", 0o644),
    ("usr/share/doc/nix-manager/README.md", "README.md", 0o644),
    ("usr/share/doc/nix-manager/screenshot.png", "screenshot.png", 0o644),
    ("usr/share/licenses/nix-manager/LICENSE", "LICENSE", 0o644),
    ("usr/share/metainfo/fr.julien68.NixManager.metainfo.xml",
     "fr.julien68.NixManager.metainfo.xml", 0o644),
]
for _px in (24, 32, 48, 64, 128, 256):
    FILE_LIST.append((
        f"usr/share/icons/hicolor/{_px}x{_px}/apps/nix-manager.png",
        f"icons/nix-manager-{_px}.png", 0o644,
    ))

# Dépendance runtime unique : Qt6 Widgets sur Solus (qt6-qtbase n'existe pas).
RUNTIME_DEPS = ["qt6-base-gui"]

OUT_FILENAME = f"{PACKAGE_NAME}-{VERSION}-{RELEASE}-{DIST_RELEASE}-{ARCH}.eopkg"

_FR_SUMMARY = ("Nix Manager - gestionnaire graphique (Qt6) de Nix sur Solus : "
               "installation de Nix, recherche et gestion des paquets")
_FR_DESCRIPTION = """nix-manager est un gestionnaire graphique (Qt6) écrit en Rust
pour utiliser Nix sur une distribution non-NixOS. Il détecte l'état de Nix
(version, démon nix-daemon, mode d'installation, profil, espace occupé par
/nix, bac à sable), installe Nix en mode démon multi-utilisateur via pkexec,
le désinstalle avec confirmation explicite, recherche dans nixpkgs, installe
un paquet ou un flake arbitraire dans le profil UTILISATEUR (sans root), liste
le profil, met à jour, nettoie le magasin et intègre les applications Nix au
menu. Toutes les commandes sont journalisées en direct et les boutons sont
désactivés pendant une opération. Binaire unique, dépendance runtime unique :
qt6-base-gui. Non officiel."""

_EN_SUMMARY = ("Nix Manager - graphical Qt6 manager for Nix on Solus: install Nix, "
               "search and manage packages")
_EN_DESCRIPTION = """nix-manager is a graphical Qt6 manager, written in Rust, for using
Nix on a non-NixOS distribution. It detects the Nix state (version, nix-daemon
service, install mode, profile, space used by the /nix store, build sandbox),
installs Nix in multi-user daemon mode through pkexec, uninstalls it with an
explicit confirmation, searches nixpkgs, installs a package or an arbitrary
flake into the USER profile (no root), lists the profile, upgrades it, cleans
the store and integrates Nix applications into the desktop menu. Every command
is streamed into a live journal and the controls are disabled while an
operation runs. Single binary, single runtime dependency: qt6-base-gui.
Unofficial."""


# ──────────────────────────────────────────────────────────────────────────────
# Helpers
# ──────────────────────────────────────────────────────────────────────────────
def sha1_hex(data: bytes) -> str:
    return hashlib.sha1(data).hexdigest()


def file_type_for(path: str) -> str:
    if path.startswith(("usr/bin/", "bin/", "usr/libexec/", "usr/sbin/", "sbin/")):
        return "executable"
    if path.startswith("usr/include/"):
        return "header"
    if path.startswith("usr/lib/"):
        return "library"
    if path.startswith("usr/share/man/"):
        return "man"
    if path.startswith("usr/share/info/"):
        return "info"
    if path.startswith("usr/share/locale/"):
        return "localedata"
    if path.startswith(("usr/share/doc/", "usr/share/gtk-doc/", "usr/share/help/")):
        return "doc"
    if path.startswith("etc/"):
        return "config"
    return "data"


# ──────────────────────────────────────────────────────────────────────────────
# Build
# ──────────────────────────────────────────────────────────────────────────────
def build():
    out_path = OUT_DIR / OUT_FILENAME
    print(f"Building: {out_path}")

    file_entries = []  # (dest_path, bytes, mode)
    for dest_path, src_rel, mode in FILE_LIST:
        src = SRC_DIR / src_rel
        if not src.exists():
            print(f"  WARNING: source file not found: {src}", file=sys.stderr)
            continue
        data = src.read_bytes()
        file_entries.append((dest_path, data, mode))
        print(f"  + {dest_path}  ({len(data)} bytes, mode {oct(mode)})")

    tar_buf = io.BytesIO()
    with tarfile.open(fileobj=tar_buf, mode="w:xz") as tar:
        for dest_path, data, mode in file_entries:
            info = tarfile.TarInfo(name=dest_path)
            info.size = len(data)
            info.mode = mode
            info.uid = 0
            info.gid = 0
            info.mtime = 0
            tar.addfile(info, io.BytesIO(data))
    install_tar_xz = tar_buf.getvalue()
    print(f"  install.tar.xz: {len(install_tar_xz)} bytes")

    files_parts = ["<Files>"]
    for dest_path, data, mode in file_entries:
        files_parts.append("  <File>")
        files_parts.append(f"    <Path>{dest_path}</Path>")
        files_parts.append(f"    <Type>{file_type_for(dest_path)}</Type>")
        files_parts.append(f"    <Size>{len(data)}</Size>")
        files_parts.append("    <Uid>0</Uid>")
        files_parts.append("    <Gid>0</Gid>")
        files_parts.append(f"    <Mode>{mode:04o}</Mode>")
        files_parts.append(f"    <Hash>{sha1_hex(data)}</Hash>")
        files_parts.append("  </File>")
    files_parts.append("</Files>")
    files_xml = "\n".join(files_parts).encode("utf-8")
    print(f"  files.xml: {len(files_xml)} bytes")

    installed_size = sum(len(data) for _, data, _ in file_entries)

    deps_xml = ""
    if RUNTIME_DEPS:
        deps_xml = "        <RuntimeDependencies>\n"
        for dep in RUNTIME_DEPS:
            deps_xml += f"            <Dependency>{dep}</Dependency>\n"
        deps_xml += "        </RuntimeDependencies>\n"

    def a(s: str) -> str:
        """ASCII strict pour les champs en anglais uniquement."""
        return s.encode("ascii", "replace").decode("ascii")

    today = date.today().isoformat()
    metadata_xml = f"""<PISI>
    <Source>
        <Name>{PACKAGE_NAME}</Name>
        <Homepage>{HOME_PAGE}</Homepage>
        <Packager>
            <Name>{PACKAGER_NAME}</Name>
            <Email>{PACKAGER_EMAIL}</Email>
        </Packager>
    </Source>
    <Package>
        <Name>{PACKAGE_NAME}</Name>
        <Summary xml:lang="en">{a(_EN_SUMMARY)}</Summary>
        <Description xml:lang="en">{a(_EN_DESCRIPTION)}</Description>
        <Summary xml:lang="fr">{_FR_SUMMARY}</Summary>
        <Description xml:lang="fr">{_FR_DESCRIPTION}</Description>
        <PartOf>{PART_OF}</PartOf>
        <License>GPL-3.0-or-later</License>
{deps_xml}        <History>
            <Update release="{RELEASE}">
                <Date>{today}</Date>
                <Version>{VERSION}</Version>
                <Comment>{PACKAGE_NAME} {VERSION} : version initiale (etat de Nix, installation/desinstallation via pkexec, recherche nixpkgs, profil utilisateur, flakes, mise a jour, nettoyage, integration au menu)</Comment>
                <Name>{PACKAGER_NAME}</Name>
                <Email>{PACKAGER_EMAIL}</Email>
            </Update>
        </History>
        <BuildHost>{BUILD_HOST}</BuildHost>
        <Distribution>{DIST_NAME}</Distribution>
        <DistributionRelease>{DIST_RELEASE}</DistributionRelease>
        <Architecture>{ARCH}</Architecture>
        <InstalledSize>{installed_size}</InstalledSize>
        <PackageFormat>1.2</PackageFormat>
        <Source>
            <Name>{PACKAGE_NAME}</Name>
            <Homepage>{HOME_PAGE}</Homepage>
            <Packager>
                <Name>{PACKAGER_NAME}</Name>
                <Email>{PACKAGER_EMAIL}</Email>
            </Packager>
        </Source>
    </Package>
</PISI>""".encode("utf-8")
    print(f"  metadata.xml: {len(metadata_xml)} bytes")

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(out_path, "w", compression=zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("metadata.xml", metadata_xml)
        zf.writestr("files.xml", files_xml)
        zf.writestr("install.tar.xz", install_tar_xz)

    size = out_path.stat().st_size
    print(f"\n  OK {out_path}  ({size} bytes)")
    print(f"  SHA1: {sha1_hex(out_path.read_bytes())}")
    return str(out_path)


if __name__ == "__main__":
    build()
