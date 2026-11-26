<img src="icon/ScreenKnob.png" width="128" alt="ScreenKnob icon">

# ScreenKnob

A small macOS menu bar app that gives **HDMI and DisplayPort screens a working volume control**:
the slider, the keyboard volume keys, and macOS's own volume popup and click sound — the same
as built-in speakers. On a Mac, monitors normally ignore the volume keys and show a "no volume
control" symbol, because their speakers have a fixed level.

It's built to stay stable when screens go to sleep, wake up or switch inputs: it notices the
change, moves sound to another speaker while the screen is away, and switches back when it returns.

## What it does

- **Volume for screens without their own control** (HDMI, DisplayPort, Thunderbolt displays).
  Speakers that do have a volume control (built-in, USB, Bluetooth) keep using it.
- **macOS volume popup and click** for those screens, through ScreenKnob's own sound driver
  (installed from the app). Without it, the volume keys still work with ScreenKnob's own popup.
- **Remembers your chosen output** and goes back to it after the screen sleeps, wakes or changes input.
- **Menu bar icon** with Louder, Quieter, Mute and the current output; closing the window keeps it running.
- Follows light and dark mode and your accent color.

## Requirements

- macOS 14.2 (Sonoma) or newer. Apple silicon or Intel.
- Optional: ScreenKnob's own sound driver (included; installed from the app) for the native popup with no
  recording indicator. No other drivers are needed.
- To build it: the Rust toolchain (free, see step 1).

## Install

### Easiest: download the app

1. Download `ScreenKnob-<version>.dmg` from the [Releases page](https://github.com/davemadan/screenknob/releases).
2. Open it and drag **ScreenKnob** onto **Applications**.
3. Open ScreenKnob from Applications. macOS will say it can't check the app (it's signed locally,
   not by Apple). Open **System Settings → Privacy & Security**, scroll down and click **Open Anyway**
   next to ScreenKnob, then confirm.
   (Or, in Terminal: `xattr -dr com.apple.quarantine /Applications/ScreenKnob.app`.)
4. Read *First launch* below.

### Or build it yourself

#### 1. Install Rust (once)

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"
```

#### 2. Build and install ScreenKnob

```sh
git clone https://github.com/davemadan/screenknob.git
cd screenknob
./scripts/make-app.sh --install
```

This builds `ScreenKnob.app` and copies it to your Applications folder. To only build it (into
`target/bundle.noindex/ScreenKnob.app`), leave out `--install`. `./scripts/make-dmg.sh` also makes the
downloadable disk image and a zip in `release/`.

### First launch

Open **ScreenKnob** from Applications (a copy you built yourself opens straight away).

For screens without a volume control, install ScreenKnob's sound driver: **Options → macOS volume popup →
Install Driver…** (your Mac password once). With it, ScreenKnob needs no permissions at all.

Without the driver, ScreenKnob asks for:

| Permission | Where | Why |
|---|---|---|
| **System Audio Recording** | Privacy & Security → Screen & System Audio Recording | ScreenKnob passes your Mac's sound through itself to set the screen's level. Nothing is recorded or saved. |
| **Accessibility** | Privacy & Security → Accessibility | So the keyboard volume keys can control the screen. |

ScreenKnob doesn't use the microphone.

### Start at login

The installed copy adds itself to **Login Items** the first time it runs, so it starts when you log in.
Switch it off or on in the ScreenKnob window (**Options → Open at login**) or in
System Settings → General → Login Items.

## Using it

- Click the speaker icon in the menu bar for Louder / Quieter / Mute, or **Open ScreenKnob…** for the window.
- In the window, pick the output under **Output device**, set the level with the slider, and switch options:
  - **Software volume** — volume for screens without their own control.
  - **Monitor's own volume** — set the screen's speakers over its cable when it allows it (no recording indicator).
  - **macOS volume popup** — routes screens through ScreenKnob's sound driver so macOS shows its own popup and
    click. While this is on, your Mac's sound output shows as "ScreenKnob"; that's expected.
  - **Volume keys** — the keyboard keys for those screens (when the popup option is off).
  - **Return to chosen output** — switch back to your screen after it sleeps, wakes or changes input.
- **Quit** from the menu bar icon. Quitting hands the sound back to your screen directly.

## About the recording indicator

macOS shows a recording indicator (next to the clock) whenever an app captures the Mac's sound. To control a
screen's volume, ScreenKnob needs the sound, and there are three ways it gets it:

1. **ScreenKnob's own sound driver (recommended, no indicator).** Like eqMac, ScreenKnob can install a small
   virtual speaker called **ScreenKnob**. macOS sends the sound there, sees a real volume control (so its own
   popup, keys and click work), and the driver hands the sound to the app through shared memory. Nothing is
   recorded, so there's no indicator. Install it from **Options → macOS volume popup → Install Driver…**
   (asks for your Mac password once; the Mac's sound restarts for a second). Remove it the same way.
2. **The monitor's own volume over the cable (no indicator).** For screens that allow it (DDC/CI), ScreenKnob sets
   the monitor's speaker volume directly (**Options → Monitor's own volume**). This needs a USB-C / Thunderbolt /
   DisplayPort connection on some Macs: the built-in HDMI port of M1 Pro/Max/Ultra Macs doesn't carry it.
3. **Capturing the sound (indicator shown).** Without the driver, ScreenKnob uses a system-audio tap. This needs the
   System Audio Recording permission, and macOS shows the indicator.

**Do I still need BlackHole?** No. Earlier versions used it; ScreenKnob now uses its own driver. If nothing else on
your Mac uses BlackHole, you can remove it: `brew uninstall blackhole-2ch`, or
`sudo rm -rf /Library/Audio/Plug-Ins/HAL/BlackHole2ch.driver && sudo killall coreaudiod`.

ScreenKnob picks the first that works for your screen.

## Troubleshooting

- **No sound after ScreenKnob was force-quit:** your output may still be set to "ScreenKnob".
  Open ScreenKnob again, or pick your screen in Control Center → Sound.
- **Volume keys don't work:** in the window, use **Reset & Ask Again** and allow ScreenKnob under Accessibility.
- **"isn't taking sound right now":** the screen is asleep or showing another input; ScreenKnob keeps
  checking and picks it up again when it's back.
- A short log is kept at `~/Library/Logs/ScreenKnob.log`.

## Uninstall

If you installed the driver, remove it first (**Options → macOS volume popup → Remove…**), or in Terminal:
`sudo rm -rf /Library/Audio/Plug-Ins/HAL/ScreenKnobAudio.driver && sudo killall coreaudiod`.
Then quit ScreenKnob from its menu bar icon and delete `/Applications/ScreenKnob.app`. To remove its
settings and log too, delete `~/Library/Application Support/ScreenKnob` and `~/Library/Logs/ScreenKnob.log`,
and remove it from the Privacy & Security lists.

## How it works

With its driver installed, ScreenKnob sets the "ScreenKnob" virtual speaker as the Mac's output. macOS sees a
speaker with a real volume control, so its own keys, popup and click work. The driver copies the sound into
shared memory, and the app plays it on the screen at that volume, gently resampling to follow the screen's
clock. Without the driver, it captures the Mac's sound with a Core Audio process tap and plays it on the
screen through a private aggregate device, scaling it by the volume you set. All audio objects are owned by
one worker thread; device changes are re-read rather than cached, and failed starts are retried, which is
what keeps it stable through sleep, wake and input switches.

Written in Rust with [egui](https://github.com/emilk/egui).

## License

MIT — see [LICENSE](LICENSE).
