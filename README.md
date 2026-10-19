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
