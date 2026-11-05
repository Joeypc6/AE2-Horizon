# Applied Energistics 2 for Horizon

An unofficial port of [Applied Energistics 2](https://github.com/AppliedEnergistics/Applied-Energistics-2) 15.4.12
(Fabric) to [Horizon](https://github.com/horizon-rust/horizon).

- `mixins/ae2`: the native mod, a Rust port of AE2's logic and screens.
- `ae2-content`: the content mod, AE2's blocks, items, recipes, textures, models and guide, converted from the 15.4.12 jar.
- `tools`: `make_content_mod.py` regenerates `ae2-content` from AE2's jar.
- `PLAN.md`: the development notes, including what was tested in each round.

Start the game with the content mod: `--dev-mod path/to/ae2-content`.

## What differs from AE2

The Java mod was rewritten in Rust for Horizon's hooks, so no original code runs. Some parts work differently inside to
behave the same in play. Parts on a cable share one block, as in AE2, through helper blocks and block entities the game
doesn't show you.

### Missing or not the same

- **Spatial storage's dimension.** Horizon mods can't add a dimension, so AE2's `spatial_storage` level doesn't exist. A
  cell with nobody in it keeps its contents in the item. A cell with a player in its volume sends them to a walled plot in
  a far, empty band of the End. Inside a plot, `/tp`, `/data`, F3 and maps say `minecraft:the_end` with x past 1,000,000,
  `execute in ae2:spatial_storage` fails, the sky has no sparkles, and its region files are in the End's folder. A swap with
  a player or an occupied plot takes up to about a second, where AE2's is instant.
- **AE2's Java API.** Java addons for AE2 can't run on this port.
- **Crafting ETA.** The time left is an estimate. It averaged 1.1% off on the test rig and was 2.0% off at worst.
- **Nested P2P error.** Worked out from the stretches between tunnels, so it is an approximation.
- **`craftingCalculationTimePerTick`.** Only applies to jobs that don't come from the confirm screen, whose plan runs on its
  own thread.
- **World height.** Parts on cables assume a world from -64 to 319.

The Fluix Researcher and its trades are there, but through mixins on tables compiled into the game, so they are the part
most likely to break on a Horizon update.

### Not tested

A real pipe from another mod into a part on a cable (no such mod exists yet; the engine lookup it would use is tested),
more than one guest at a time, and speed against the Java mod.

`PLAN.md` has the full history; Round 22 is the current list.

## Build

This is a [Horizon](https://github.com/horizon-rust/horizon) native mod bundle: Rust mixins compiled into the game.
It was built and tested on Horizon commit `6475f62` (Minecraft 26.3, native hooks API 359), as `native-6475f62-d72af06acdd9`.
Native mods are unsandboxed code that runs with your rights, so read what you build.

Put each bundle's `mixins/<id>` folder in one folder, then:

```bash
horizon-launcher native-build "path/to/that/folder" --trust-native-code
```

## Status

An unofficial port, not made or endorsed by the original authors. Please report problems here, not to them.
It was tested on Windows, in single player, on LAN with one guest, and on a dedicated server with one client. Other platforms were not tested.
A Horizon update can break it until it is rebuilt: it is built for the one Horizon commit named under Build.

## Credits and licences

- Applied Energistics 2, (c) 2013 - 2020 AlgorithmX2 et al and TeamAppliedEnergistics.
- Code: this port is a modified version of AE2's code, under the [GNU LGPL 3.0](LICENSE) (with [GPL 3.0](COPYING.GPL-3.0)).
- Textures and models in `ae2-content`, (c) 2020 [Ridanisaurus Rid](https://github.com/Ridanisaurus/), (c) 2013 - 2020
  AlgorithmX2 et al: [CC BY-NC-SA 3.0](https://creativecommons.org/licenses/by-nc-sa/3.0/). Converted to Horizon's
  formats; noncommercial use only.
- Text and translations: [CC0](https://creativecommons.org/publicdomain/zero/1.0/).
- Guidebook click sound, [EminYILDIRIM](https://freesound.org/people/EminYILDIRIM/sounds/536108/):
  [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
