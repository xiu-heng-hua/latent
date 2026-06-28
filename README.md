# Latent

A small, readable RAW developer. The workspace builds a single user-facing
binary named `latent` (the GUI editor and a `develop` CLI subcommand) on top of a
set of focused library crates.

## Build and run

```sh
cargo build --release
cargo run -p latent-app            # opens the editor on the welcome screen
cargo run -p latent-app -- photo.nef   # opens straight into a RAW
```

The display name shown in the title bar, the About dialog, and the welcome
screen is **Latent**; the binary, the CLI command, and the window application id
all use the lowercase technical id **`latent`**.

## Desktop integration (Linux)

The window sets its application id to `Latent`. On GNOME/Wayland the taskbar
icon and name come from a `.desktop` file matched to that id (not from the
in-window icon), so a bare `cargo run` shows a generic icon (the id itself,
`Latent`, stands in for the name) until the desktop entry is installed.

To get the proper taskbar icon and name, install the bundled desktop entry and a
PNG icon into the per-user locations:

```sh
# 1. The application binary on PATH (release build, or a symlink to it).
install -Dm755 target/release/latent ~/.local/bin/latent

# 2. The desktop entry (its filename stem must match the app id: Latent).
install -Dm644 latent-app/assets/Latent.desktop \
  ~/.local/share/applications/Latent.desktop

# 3. A PNG icon named after the Icon= key (latent). A 256×256 PNG is a good size.
install -Dm644 latent-app/assets/icon.png \
  ~/.local/share/icons/hicolor/256x256/apps/latent.png

# 4. Refresh the caches so the desktop picks it up.
update-desktop-database ~/.local/share/applications 2>/dev/null || true
gtk-update-icon-cache ~/.local/share/icons/hicolor 2>/dev/null || true
```

After this the taskbar shows the **Latent** name and the bundled icon, and the
entry appears in the application launcher under Graphics / Photography.
