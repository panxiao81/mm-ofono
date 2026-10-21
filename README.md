# mm-ofono

Yet Another oFono-to-ModemManager Adapter, Rewritten in Rust™.

Exposes the ModemManager D-Bus API (`org.freedesktop.ModemManager1`) on top of
oFono, so ModemManager clients (NetworkManager, Phosh, GNOME Calls, Chatty,
gnome-control-center, …) work on Halium/Droidian devices whose modem is driven
by oFono + ofono-binder. Replaces the Python `ofono2mm`.

Every exported interface is checked against ModemManager's own introspection
XML (`tests/conformance.rs`).

## Development

Builds and tests run in a container matching the Droidian build image:

    dev/cargo test
    dev/cargo build --release

## Packages

CI builds an arm64 `.deb` in the Droidian build container on every push to
`main` and publishes it as a GitHub release. The package replaces `ofono2mm`
(`Provides`/`Conflicts`/`Replaces`) and runs `/usr/bin/mm-ofono` as
`ModemManager.service` through a drop-in. Tag `droidian/<version>` to set the
version.

## License

GPL-2.0-or-later, see `LICENSE`.
