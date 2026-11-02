# DataShield

A password manager that lives in your terminal. Every secret stays on your
machine, encrypted with the algorithm you pick. No cloud, no account, no
subscription.

```sh
curl -fsSL https://datashield.aeratic.com/install | sh -s -- tui
```

macOS and Linux today; Windows coming.

## In this repository

| Folder | What |
| --- | --- |
| `DataShield-Core/` | The vault format, cryptography, sync and data model shared by every front-end |
| `DataShield-TUI/` | The terminal interface |
| `DataShield-GUI/` | The graphical interface (coming) |
| `extension/` | Browser extension (Chrome, Firefox) for login assist |
| `dist/` | Packaging: installer, macOS bundle, Linux launcher |
| `docs/` | Storage standard, login assist, security notes |

## Build from source

```sh
cargo build --release -p datashield-tui     # → target/release/datashield-tui
dist/macos/build-app.sh                     # macOS: → target/macos.noindex/DataShield.app
```

See `dist/README.md` for the Linux launcher and `docs/STORAGE-STANDARD.md` for
the vault format.

## License

GPL-3.0-or-later. DataShield's macOS bundle ships a copy of the
[kitty](https://github.com/kovidgoyal/kitty) terminal, also GPLv3.
