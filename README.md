# Glitch Mangler

A tempo-synced random stutter/glitch CLAP effect written in Rust with
[nice-plug](https://codeberg.org/RustAudio/nice-plug).

On every grid step the plugin rolls a dice (`Chance`). When it hits, it picks one
of the effects below (weighted by their knobs) and runs it for a random number
of steps, up to `Max Length`. `Stack` is the chance that Crush, Gate, or Rebound
cuts in and out, in rhythm, over a Stutter, Reverse, Tape Stop, or Scramble.
`Haunt` is the chance that a glitch replays the last glitch exactly, so a
stutter from one bar comes back in a later one. Both start at 0%. The line under the title names the latest
glitch at the bar and beat where it started:

| Effect    | What it does                                                      |
| --------- | ----------------------------------------------------------------- |
| Stutter   | Loops a slice from the step (1/1–1/8 of it), sometimes pitched or shrinking into a roll |
| Reverse   | Plays the preceding audio backwards                               |
| Tape Stop | Slows playback down to a halt                                     |
| Scramble  | Jumps back 1–8 steps and replays that audio                       |
| Crush     | Bit reduction and sample-rate reduction                           |
| Gate      | Rhythmic chopping at the step rate                                |
| Rebound   | Cutoff snaps twice per grid step from open to shut and back, and some steps ring into near silence |

With `Lock to Song` on (default), the randomness is derived from the song
position and `Seed`, so the same part of the arrangement glitches identically
on every playback and bounce. Change `Seed` to get a different pattern. When the
transport is stopped, the plugin runs on an internal clock at the host tempo.

## Build and install (Linux)

```sh
cargo build --release
ln -sfn "$PWD/target/release/libaudio_plugin.so" ~/.clap/glitch_mangler.clap
```

Then rescan plugins in Bitwig Studio (Settings → Plug-ins) and add
**Glitch Mangler** to a track. Rebuilding updates the plugin through the
symlink, but reload the device in Bitwig to pick up the change.

## Tests

```sh
cargo test --release
```
