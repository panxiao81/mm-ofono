# mm-ofono

Yet Another oFono-to-ModemManager Adapter, Rewritten in Rust™.

Exposes the ModemManager D-Bus API (`org.freedesktop.ModemManager1`) on top of
oFono, so ModemManager clients (NetworkManager, Phosh, GNOME Calls, Chatty,
gnome-control-center, …) work on Halium/Droidian devices whose modem is driven
by oFono + ofono-binder. Replaces the Python `ofono2mm`.

Every exported interface is checked against ModemManager's own introspection
XML (`tests/conformance.rs`).

## Semantics

mm-ofono translates and keeps no state or policy of its own: where oFono
or NetworkManager has the wrong semantics, the fix belongs there. It
follows real ModemManager's behaviour, not ofono2mm's; for example a SIM
event removes and re-exports the modem as MM's reprobe does, because
NetworkManager relies on it.

The one deliberate extension: **each SIM slot is its own modem.**
ModemManager has no way to use two SIMs at once (one modem, `SimSlots`,
only the primary SIM active), while a DSDS phone keeps both lines
registered for calls, SMS and IMS. mm-ofono exports every oFono modem
(`/ril_0`, `/ril_1`, ...) as a separate ModemManager modem, Modem/0 for
slot 1 and so on, kept across reprobes, so clients list SIMs in slot
order. `SimSlots` stays empty and `SetPrimarySimSlot` unsupported.

Not available because oFono has no equivalent: bands, capabilities,
reset, AT commands, SMS storage, preferred networks, GPS, Firmware, OMA,
SAR, CDMA, Inhibit.

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
