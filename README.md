# future_rust_cop

This project is a fork of [RadiatorTwo/future_rust_cop](https://github.com/RadiatorTwo/future_rust_cop/tree/main).
The original project and its contributors provide the recompiler and platform work on which
this fork is built.

**Future Cop: L.A.P.D.** (1998) running natively on modern Windows, Linux and macOS, written in Rust.

fcop takes the original game's program and recompiles it into native code when it starts,
using [iced-x86] and [Cranelift]. Everything the game expects from Windows and DirectX
(graphics, sound, videos, network play) is rebuilt in Rust, with OpenGL for the 3D graphics.

> **This project contains no game files. You need your own copy of the game.**

![A mission in widescreen at 1920x1080](docs/screenshots/game.png)

<table>
  <tr>
    <td width="33%"><img src="docs/screenshots/coop.png" alt="Two players in split screen"></td>
    <td width="33%"><img src="docs/screenshots/precinct.png" alt="Precinct Assault"></td>
    <td width="33%"><img src="docs/screenshots/menu.png" alt="The main menu"></td>
  </tr>
  <tr>
    <td align="center"><em>Two players in split screen</em></td>
    <td align="center"><em>Precinct Assault</em></td>
    <td align="center"><em>The main menu</em></td>
  </tr>
</table>

## Contents

- [Changes in this fork](#changes-in-this-fork)
- [How to play](#how-to-play)
- [Menu music](#menu-music)
- [What is new](#what-is-new)
- [Modern graphics effects](#modern-graphics-effects)
- [Gamepads](#gamepads)
- [Network play](#network-play)
- [Replacement textures](#replacement-textures)
- [Sounds export](#sounds-export)
- [Windows launcher](#windows-launcher)
- [Building](#building)

## Changes in this fork

The main additions and changes in this fork include:

- **Controller input:** smoother menu navigation with directional repeat and stick
  hysteresis; modern and classic console profiles, secondary bindings, per-player device
  selection, and Xbox/PlayStation button glyphs.
- **Controller vibration:** optional feedback for damage and mode changes on compatible
  controllers on Windows and Linux. The Windows DualSense joystick fallback provides input
  only, and the current macOS backend does not support vibration. See [Gamepads](#gamepads).
- **Autosave and menus:** automatic menu-progress saves separate from manual slots,
  a skip-intro preference, and updated Help and credits access.
- **Windows launcher:** a blue, tabbed interface for installation, game settings, backups,
  texture import/export and sound export. It prepares and validates the compiled cache
  before starting the game. Its modern game-dialog presentation is a preview.
- **Cache and installation safety:** cache-only startup and validation, disc-image preflight,
  staged launcher installation, and tests for cache, installation and save-management paths.

For setup and limitations, see [Windows launcher](#windows-launcher) and
[Gamepads](#gamepads).

## How to play

### Requirements

- The PC version of the game, **US** or **German**. fcop tells you if your version is not
  supported.
- Windows or Linux (64-bit) with OpenGL 3.3, or macOS on Apple Silicon.
- On Linux you also need ALSA (or PipeWire/PulseAudio) for sound and libudev for gamepads.
  Every normal desktop distribution has them.

### Installation

1. **Download** the latest version from the [releases] page and unpack it:
   `fcop-...-windows-x64.zip` for Windows, `fcop-...-linux-x64.tar.gz` for Linux.
   On macOS, see [macOS](#macos) below.
2. **Add the game.**

   Choose **one** of these ways:

   | Option | What you have | What to do |
   | --- | --- | --- |
   | A | an image of your CD (`.iso` or `.bin`) | put it next to `fcop.exe` (Linux: `fcop`) or into `game_files` |
   | B | the CD itself | copy everything on it into `game_files` |
   | C | the game installed on your PC | copy the game's folder into `game_files` |

3. **Start** `fcop.exe` (Linux: run `./fcop` in its folder). On Windows, keep
   `fcop-launcher.exe` beside it; the launcher opens if the game hasn't been prepared.

On Linux and macOS, the first start finds the game, checks it and sorts out what it needs:

| Folder | What goes there |
| --- | --- |
| `game_files/FCopLAPD` | `FCopLAPD.exe`, `febmp.bin` and the `missions` folder |
| `game_files/Video` | the videos |

Everything else in `game_files` is moved to the recycle bin (on Linux and macOS it is deleted).

On Windows, the launcher prepares and validates compiled game code before launching. On
other platforms, fcop compiles the game on first start. Later starts reuse the compiled cache.

### macOS

fcop for macOS (Apple Silicon) is an app.

1. **Download** `fcop-...-macos-arm64.dmg` from the [releases] page and double-click it. A
   window opens with `fcop.app` and your Applications folder: **drag fcop onto Applications**.
   Start it from there (started from the Downloads folder, macOS does not run it).
2. **Start** fcop. The app is not signed with a developer certificate, so macOS blocks the
   first start. Open **System Settings > Privacy & Security** and click **Open Anyway**.
3. **Add the game.** The window says that the game files were not found. Click **Open
   Folder**, put your CD image (or a copy of the CD, or your installed game folder) into it,
   and start fcop again.

The app keeps its files in `~/Library/Application Support/fcop`: the game and your settings in
`game_files`, the log in `fcop.log`.

### Where your files are

| What | Where |
| --- | --- |
| Settings | `game_files/FCopLAPD/registry.txt` |
| Key bindings | `game_files/FCopLAPD/controls.txt` |
| Gamepad bindings | `game_files/FCopLAPD/gamepad.txt` |
| Secondary gamepad bindings | `game_files/FCopLAPD/gamepad-secondary.txt` |
| Saved games | `game_files/FCopLAPD/save.cfg` |
| Log (if something goes wrong) | `fcop.log` next to `fcop.exe` on Windows, the terminal on Linux and macOS |

## Menu music

The PC version has no music in the menus. You can add the music from the PlayStation version.

Choose **one** of these ways:

| Option | What you have | What to do |
| --- | --- | --- |
| A | nothing | download `FE.MIS` from [PCGamingWiki](https://community.pcgamingwiki.com/files/file/4212-music-file-future-cop-lapd-from-psx/) and put it into `game_files/PS1` |
| B | an image of the PlayStation CD (`.bin` with `.cue`, or `.iso`) | put the image into `game_files` |
| C | the PlayStation CD itself | copy `CW/FE.MIS` from the CD into `game_files/PS1` |

The music plays at the game's Music Volume.

## What is new

**Graphics**
- High resolutions and widescreen
- Fullscreen (Alt+Enter)
- Anti-aliasing and sharper texture filtering
- Higher frame rates, up to unlimited, without changing how the game plays
- Videos as large as the window allows
- [Replacement textures](#replacement-textures) (HD textures)

You find all of this in the game's Preferences. During a mission, "Display" in the pause menu
opens them too, and changes show right away.

**Controls**
- Player 1 plays with the arrow keys
- A new camera close behind the robot
- [Gamepads](#gamepads), in the menus too
- Mouse buttons can be assigned to actions, including the side buttons

  In the Keyboard dialog, select an action and press the mouse button you want. For the left
  button, click the selected action a second time.

**More**
- [Modern graphics effects](#modern-graphics-effects): shadows, lighting and more
- [Network play](#network-play) over the local network or the internet
- [Sounds export](#sounds-export): all sounds and music as WAV files
- Autosave: your menu progress is saved automatically after each mission to `game_files/FCopLAPD/autosave.cfg`, separate from the manual saves in `save.cfg`. It is loaded when the game starts; loading a manual save overrides it.
- **Skip Intro Videos** in Preferences (off by default) skips the logo and intro videos at startup.
- **Help** in the menu offers the help video, the credits video or the PlayStation manual (opens in your browser). The credits no longer play when you quit.

## Modern graphics effects

fcop can add effects the original never had. They are all optional, and you can turn each one
on or off while playing.

You find them in Preferences, in the 3DFX Options, behind the button **Advanced Graphics...**.

| Effect | What it does |
| --- | --- |
| Ambient Occlusion | soft shading in corners and where objects meet the ground |
| Dynamic Lights | explosions, muzzle flashes, rockets and police lights light up their surroundings |
| Lit Smoke | smoke is lit by those lights (needs Dynamic Lights) |
| Heat Distortion | the air shimmers above fire and explosions |
| Bloom | bright lights glow |
| Perspective-Correct Textures | textures no longer warp on large surfaces |
| Depth Buffer | fixes things showing through each other (on by default) |
| Shadows | real shadows from the sun: Screen-Space or Shadow Map |
| Tonemapping | richer bright colours: Soft, AgX or ACES |

**The PlayStation look:** three more boxes sit right in the 3DFX Options, below the gamma
slider. They are on by default.

| Option | What it does |
| --- | --- |
| Texture Coloring | lit textures can get up to twice as bright (the PC version cut them off) |
| PlayStation Colors | the first mission's textures in their PlayStation colours |
| PlayStation Shading | the contrast of the original software renderer |

## Gamepads

Up to two gamepads are supported, and you can plug them in while the game is running. They
work alongside the keyboard. You change their buttons with "Gamepad" in Preferences or in the
pause menu.

The default **Console (modern)** layout (Xbox controller):

| Button | Action |
| --- | --- |
| Left stick | move forward/back and strafe left/right |
| Right stick / D-pad left/right | turn left/right |
| X | gun |
| A | heavy weapon |
| B | special |
| Y | jump |
| LB | action |
| RB | change target |
| LT / RT | jink left / right |
| Back | map |
| R3 | camera |
| Start | pause |

The left stick is analog: push it a little and the robot moves slowly. (Not in network games.)
The **Console (classic)** profile instead turns with the left stick or D-pad and strafes
with LT/RT. The PlayStation transform, crouch, and short-jink button combinations are
not implemented.

Existing `gamepad.txt` bindings are preserved. To apply either layout, open **Gamepad**,
choose the default profile, and select **Console (modern)** or **Console (classic)**.
Saving the dialog also replaces that player's secondary bindings with the chosen profile's
defaults. Open **Gamepad** and choose **Secondary bindings...**
to edit alternate bindings. Save changes with OK before switching binding pages.
Both bindings work alongside the keyboard. The dropdown under each player in the
**Gamepad** dialog selects a connected controller without changing
that player's bindings. **Automatic** uses the connection order, and **Disabled** turns off
controller input for that player. Selections last for the current session; select them
again after restarting. The glyph style selector in the Gamepad dialog offers **Auto**,
**Xbox**, and **PlayStation**; Auto uses PlayStation glyphs for recognized Sony controllers
and Xbox glyphs for other controllers.

In menus and dialogs, the D-pad or left stick moves, A selects and B goes back.
Holding a direction repeats; stick hysteresis helps prevent accidental changes near diagonals.

When your robot takes damage or changes mode, compatible controllers can vibrate on Windows
and Linux. Turn off **Vibration** in the **Gamepad** dialog to disable it. `FCOP_RUMBLE=0`
also starts the game with vibration off. The current macOS gamepad backend does not support
vibration. Standard controller input depends on the operating system and gilrs mappings.
On Windows, Bluetooth DualSense controllers can also use the Windows joystick fallback when
gilrs does not detect them. This fallback provides input only: DualSense vibration, adaptive
triggers and advanced haptics are not supported through it.

## Network play

One player chooses **Host Game**, the others **Join Game**.

Only the host has to open these ports:

| Port | What it is for |
| --- | --- |
| TCP 47624 | the game itself |
| UDP 47624 | finding games on the local network |

The players who join don't need to open anything.

### Firewall (Windows)

The first time you host, Windows asks if fcop may use the network. Allow it for private
networks.

If you clicked "no", open PowerShell as administrator and run:

```powershell
New-NetFirewallRule -DisplayName "Future Cop TCP" -Direction Inbound -Protocol TCP -LocalPort 47624 -Action Allow -Profile Private
New-NetFirewallRule -DisplayName "Future Cop UDP" -Direction Inbound -Protocol UDP -LocalPort 47624 -Action Allow -Profile Private
```

### The game is not found

- **Different network or VPN:** type the host's IP address into "Host Address" in the Join
  dialog.
- **Over the internet:** the host's router has to forward TCP and UDP port 47624 to the
  host's computer.

## Replacement textures

You can replace the game's textures with your own, for example sharper HD versions.

### How it works

The game keeps its textures on pages of 256x256 pixels, each full of small pictures. Every
page has its own name, a code like `0123456789abcdef`.

To replace a page, put a PNG with that code in its name into `game_files/Textures` (subfolders
are fine):

- the name can start with anything, e.g. `street_0123456789abcdef.png`
- transparency works, soft edges too
- up to 4 times larger (1024x1024)

To turn replacements off, start fcop with `FCOP_HD=0`.

### Making your own

1. **Export:** run `fcop export` in a terminal in fcop's folder.

   This cuts every page into its single pictures, e.g. a part of the robot or a piece of the
   street. You find them in `game_files/Textures/pieces`, one folder per page. Each folder
   also has:
   - `page.png`: the whole page
   - `layout.png`: where each picture is on the page
   - a `.json` file per picture, showing where it is used on the model. Handy if you want to
     redraw it from scratch.
2. **Edit** the pictures in any image editor. Keep the file names. They may be up to 4 times
   larger.
3. **Import:** run `fcop import`. Your pictures are put back into pages, and the game uses
   them from its next start.

Running `fcop export` again does not overwrite your edited pictures.

### texview

`tools/texview` is a small viewer for the exported pictures. Build it with
`cargo build --release` in its folder, then open the `pieces` folder with it (or drag a folder
onto it).

It shows the pictures by mission, how they sit on the model, the original next to your
version, and where each one is on its page. Pictures you changed are marked orange. When you
save a picture in your image editor, texview shows the new version at once.

| Key | Action |
| --- | --- |
| Up / Down | previous / next folder |
| Left / Right | previous / next picture |
| G | show grid |
| O | show original |
| E | open in image editor |
| + / - / 0 | zoom |
| Ctrl+F | search |

### Textures the game makes while playing

A few textures only appear during play (like the HUD's message icon). To get them, start fcop
with `FCOP_DUMP_TEXTURES=1` and play. Every new texture is saved to `game_files/Textures/dump`.

## Sounds export

Run `fcop export-sounds` in a terminal in fcop's folder. It saves all sound effects, voices
and music of the game to `game_files/Sounds` as WAV files. `sounds.txt` lists which missions
use each sound.

## Windows launcher

Build the launcher and game together with `cargo build --release --bin fcop-launcher --bin fcop`,
then put both executables in the same folder. Open `fcop-launcher.exe` to choose a PC
disc image; supported ISO, BIN and IMG images beside the launcher are detected automatically.
The source image is not moved or deleted. An existing installation is used without being
overwritten; otherwise game files are installed into `game_files`.

To use PlayStation menu music, enable the checkbox and select a PS1 disc image or `FE.MIS`.
Existing imported music is not overwritten. Uncheck the box to disable music for that launch.

The launcher has **General**, **Video**, **Gamepad**, **Saves**, and **Tools** tabs. Disc, music and
dialog-preview choices are saved in `fcop-launcher.cfg` beside the launcher when you press
Play or close the window; the folder must be writable.

Game preferences are read from `game_files/FCopLAPD/registry.txt`. Close the game, then
select **Save** beside Play to save the launcher choices and game settings from all sections.
**Reload game settings** discards unsaved game-setting edits and reads the file again. Video options
include resolution (`window` or `WIDTHxHEIGHT`), frame-rate limit, fullscreen, V-Sync,
anti-aliasing and filtering. General includes skip intro; Gamepad includes vibration.
Configure individual gamepad bindings in the game.

**Read game data...** shows the status and sizes of save and binding files. It does not
decode individual save slots. The **Tools** tab exports textures and sounds and imports
edited textures to the same `game_files/Textures` and `game_files/Sounds` folders used by
the command-line tools.

When you press Play, the launcher checks the compiled cache, prepares it if necessary, and
validates it before starting the game. The launcher closes after the game process starts.
If a direct Windows launch finds no compatible cache, it opens the launcher instead.
Direct launches also read the saved launcher music and dialog choices.

**Save management** backs up and restores manual saves, autosave, save state, game settings,
and keyboard and gamepad bindings. Backups are stored in `fcop-backups` beside the launcher.
Restore backs up existing files first, then replaces files present in the selected backup;
files absent from that backup are left unchanged. Resetting gamepad bindings also creates a
backup first. Close the game before backing up, restoring or resetting bindings.

The launcher currently supports Windows. Its **Modern dialogs (preview)** option changes
the presentation of game dialogs while retaining their existing layout and actions; the
main menus remain classic. A complete modern game interface and a fully compilation-free
game executable are not yet available.

## Building

```
cargo build --release
cargo run --release -- path/to/FCopLAPD.exe
```

Without a path, fcop uses the `game_files` folder as described above. The tests
(`cargo test --release`) need no game files.

For development, `fcop --cache-only path/to/FCopLAPD.exe` requires a compatible
`compiled.bin` generated by the same fcop executable. It refuses to recompile guest
functions if the cache is missing or incompatible. `--prepare path/to/FCopLAPD.exe`
creates the cache, and `--validate-cache path/to/FCopLAPD.exe` checks it without starting
the game. Runtime hook stubs and trampolines are still compiled, so this is not a fully
compilation-free executable.

### macOS app

`tools/macos-app.sh` builds `target/fcop.app` and packs it into one file to share:
`target/fcop-<version>-macos.dmg`. How players use it: [macOS](#macos).

GitHub builds the app of a branch when the workflow **macOS app** is started by hand
(`gh workflow run macos-app.yml --ref <branch>`). To get it, open **Actions > macOS app**, then
that run, and download it under **Artifacts** (you must be
signed in to GitHub). The download is a zip that holds the app's zip.

How fcop works inside and what is still open: `CLAUDE.md` and `PLAN.md`.

[iced-x86]: https://github.com/icedland/iced
[Cranelift]: https://cranelift.dev
[releases]: https://github.com/RadiatorTwo/future_rust_cop/releases
