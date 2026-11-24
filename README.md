# future_rust_cop

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

- [How to play](#how-to-play)
- [Menu music](#menu-music)
- [What is new](#what-is-new)
- [Modern graphics effects](#modern-graphics-effects)
- [Gamepads](#gamepads)
- [Network play](#network-play)
- [Replacement textures](#replacement-textures)
- [Sounds export](#sounds-export)
- [Building](#building)

## How to play

### Requirements

- The PC version of the game, **US** or **German**. fcop tells you if your version is not
  supported.
- Windows or Linux (64-bit) with OpenGL 3.3, or macOS on Apple Silicon.
- On Linux you also need ALSA (or PipeWire/PulseAudio) for sound and libudev for gamepads.
  Every normal desktop distribution has them.

### Installation

1. **Download** the latest version from the [releases] page and unpack it:
   `fcop-...-windows-x64.zip` for Windows, `fcop-...-linux-x64.tar.gz` for Linux,
   `fcop-...-macos-arm64.tar.gz` for macOS.
2. **Add the game.**

   Choose **one** of these ways:

   | Option | What you have | What to do |
   | --- | --- | --- |
   | A | an image of your CD (`.iso` or `.bin`) | put it next to `fcop.exe` (Linux, macOS: `fcop`) or into `game_files` |
   | B | the CD itself | copy everything on it into `game_files` |
   | C | the game installed on your PC | copy the game's folder into `game_files` |

3. **Start** `fcop.exe` (Linux, macOS: run `./fcop` in its folder).
   On macOS the download is not signed. If macOS blocks fcop, run
   `xattr -dr com.apple.quarantine .` in its folder once.

On the first start fcop finds the game, checks it and sorts out what it needs:

| Folder | What goes there |
| --- | --- |
| `game_files/FCopLAPD` | `FCopLAPD.exe`, `febmp.bin` and the `missions` folder |
| `game_files/Video` | the videos |

Everything else in `game_files` is moved to the recycle bin (on Linux and macOS it is deleted).

The first start takes a second or two longer, because fcop compiles the game once. After that
it starts right away.

### Where your files are

| What | Where |
| --- | --- |
| Settings | `game_files/FCopLAPD/registry.txt` |
| Key bindings | `game_files/FCopLAPD/controls.txt` |
| Gamepad bindings | `game_files/FCopLAPD/gamepad.txt` |
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

The default layout (Xbox controller):

| Button | Action |
| --- | --- |
| Left stick | move |
| Right stick | turn |
| RT | gun |
| LT | heavy weapon |
| A | jump |
| B | special |
| X | action |
| Y | change target |
| LB / RB | jink |
| Back | map |
| R3 | camera |
| Start | pause |
| D-pad | free to use |

The sticks are analog: push them a little and the robot moves slowly. (Not in network games.)

In menus and dialogs, the D-pad or left stick moves, A selects and B goes back.

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

## Building

```
cargo build --release
cargo run --release -- path/to/FCopLAPD.exe
```

Without a path, fcop uses the `game_files` folder as described above. The tests
(`cargo test --release`) need no game files.

How fcop works inside and what is still open: `CLAUDE.md` and `PLAN.md`.

[iced-x86]: https://github.com/icedland/iced
[Cranelift]: https://cranelift.dev
[releases]: https://github.com/RadiatorTwo/future_rust_cop/releases
