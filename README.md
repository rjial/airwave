# Airwave

Stream your computer's sound to other devices on your network, or play sound from them.

A self-hosted audio relay with its own wire protocol:

- **Server** (desktop) — Rust + [gpui](https://crates.io/crates/gpui), captures and streams system audio.
- **Client** (Android) — Kotlin + Jetpack Compose, plays the stream and can send its mic back.

## Layout

```
.                 # Rust desktop server (Cargo)
├── src/          # server source
└── android/      # Android client (Gradle)
```

## Build

### Server (Linux)

System deps (Debian/Ubuntu): `libasound2-dev`, `libfontconfig-dev`, `libwayland-dev`,
`libxkbcommon-x11-dev`, `libvulkan-dev` plus a C toolchain. Then:

```bash
cargo build --release
./target/release/airwave
```

### Client (Android)

Requires JDK 17 and the Android SDK. For the native audio backends (OpenSL ES and Oboe),
install the NDK and CMake via the SDK Manager:

```bash
sdkmanager "ndk;28.2.13676358" "cmake;3.22.1"
cd android
./gradlew assembleDebug
```

Without the NDK the app still builds and falls back to `AudioTrack`.

## Audio output backends (client)

`AudioTrack`, `AudioTrack · low latency`, `AudioTrack · power saving`, `OpenSL ES`, and
`Oboe` (AAudio under the hood, lowest latency on modern devices).

## Releases

Push a semver tag to build and publish a GitHub Release (Linux server binary + unsigned
Android APK):

```bash
git tag v0.1.0
git push origin v0.1.0
```

The Android APK is unsigned — sign it before distributing on the Play Store.
