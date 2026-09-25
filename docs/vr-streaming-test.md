# Testing VR before and with the headsets (Linux)

Render machine: this desktop (`rocksteady`, Intel Arc B580, Ubuntu 26.04, Wayland). The laptop
is used for USB work with the headset (installing APKs, logs); it's too weak to render VR.

## Tools

| Tool | Version | Role | Installed |
|---|---|---|---|
| Monado | 25.0.0 (apt) | Open-source OpenXR runtime; its `qwerty` driver simulates a headset and controllers with keyboard and mouse | needs `sudo apt install` (below) |
| WiVRn server | 26.9 (Flathub, user install) | OpenXR runtime that renders on this PC, encodes on the GPU and streams to the Quest | yes: `flatpak run io.github.wivrn.wivrn` |
| WiVRn client | 26.9 APK, arm64 | Runs on the Quest, receives the stream | `~/vr-tools/WiVRn-26.9-client.apk` |
| `hello_xr` | `libopenxr-utils` | Khronos sample OpenXR app; smoke test for both runtimes | needs `sudo apt install` |
| `vr_fire_quest.apk` | this repo | Standalone Quest build (no streaming) | `unity/Build/Quest/` |

One-time setup that needs sudo:

```sh
sudo apt install -y monado-service monado-cli libopenxr1-monado libopenxr-loader1 libopenxr-utils adb
sudo ufw allow 9757/tcp comment WiVRn
sudo ufw allow 9757/udp comment WiVRn
```

## Unity can't be the PC-side app on Linux

Unity's OpenXR plugin supports only Windows x64 and macOS for desktop builds ("The only
standalone targets supported are Windows x64 and OSX with OpenXR"). A Unity Linux player can't
run on Monado or WiVRn. For the server-rendered path there are three options:

1. **Windows render machine** with the Unity desktop build, streamed with Meta Air Link /
   Quest Link, Steam Link or ALVR.
2. **Linux render machine with a non-Unity OpenXR app**, streamed by WiVRn. The Bevy viewer
   could gain an OpenXR mode (`bevy_mod_openxr`).
3. **Our own streaming**: the server renders both eyes from head poses the headset sends, with
   no OpenXR on the server. It encodes the frames and sends them to a Quest client (the Unity
   Android build, which does support OpenXR), which shows them with reprojection. WiVRn is a
   ready-made version of this and gives us numbers to beat.

## Before the headsets: simulated headset (Monado)

```sh
# Terminal 1: Monado with the keyboard/mouse headset (focus its window to steer)
QWERTY_ENABLE=1 monado-service
# Terminal 2: the sample app on the Monado runtime
XR_RUNTIME_JSON=/usr/share/openxr/1/openxr_monado.json hello_xr -g Vulkan
```

`XR_RUNTIME_JSON` picks the runtime for one command, so Monado and WiVRn never fight over
the system-wide `active_runtime.json`.

## With the headsets

**Standalone (on the headset's own GPU):** plug the Quest into the laptop, turn on developer
mode (Meta Horizon phone app), allow USB debugging in the headset, then

```sh
adb install -r unity/Build/Quest/vr_fire_quest.apk      # Library > Unknown Sources
```

**Streamed (rendered on this PC):**

1. `adb install -r ~/vr-tools/WiVRn-26.9-client.apk` (or let the WiVRn dashboard install it).
2. Start the server here: `flatpak run io.github.wivrn.wivrn`. Put the headset on the same
   network (5 GHz or 6 GHz Wi-Fi, ideally with the PC on Ethernet), open WiVRn on the Quest,
   and pick `rocksteady`.
3. Run an OpenXR app here, e.g. `XR_RUNTIME_JSON=~/.local/share/flatpak/app/io.github.wivrn.wivrn/current/active/files/share/openxr/1/openxr_wivrn.json hello_xr -g Vulkan`
   (WiVRn also sets itself as the active runtime while a headset is connected).
4. Note the encoder (AV1/HEVC on the B580), bitrate, and latency the WiVRn dashboard reports.

Compare the standalone and streamed runs for frame rate, latency and comfort.
