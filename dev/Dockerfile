# Dev/test environment matching the Droidian build image (same rustc/cargo as CI).
FROM quay.io/droidian/build-essential:next-arm64
RUN apt-get update \
 && apt-get install -y --no-install-recommends cargo rustc modemmanager-dev dbus-daemon \
 && rm -rf /var/lib/apt/lists/*
