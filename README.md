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
behave the same in play. A cable with facades is one of 768 helper blocks, because Horizon sets a block's collision per
block state. Parts on a cable are separate blocks. What is still not 1:1 is listed under Left in `PLAN.md` (Round 17 has the current list).

## Build

This is a [Horizon](https://github.com/horizon-rust/horizon) native mod bundle: Rust mixins compiled into the game.
It was built and tested on Horizon commit `c9d474d` (Minecraft 26.3, native hooks API 80), as `native-c9d474d-6181d2a7fb1b`.
Native mods are unsandboxed code that runs with your rights, so read what you build.

Put each bundle's `mixins/<id>` folder in one folder, then:

```bash
horizon-launcher native-build "path/to/that/folder" --trust-native-code
```

## Status

An unofficial port, not made or endorsed by the original authors. Please report problems here, not to them.
It was tested on Windows, in single player and on LAN with one guest. Other platforms and dedicated servers were not tested.

## Credits and licences

- Applied Energistics 2, (c) 2013 - 2020 AlgorithmX2 et al and TeamAppliedEnergistics.
- Code: this port is a modified version of AE2's code, under the [GNU LGPL 3.0](LICENSE) (with [GPL 3.0](COPYING.GPL-3.0)).
- Textures and models in `ae2-content`, (c) 2020 [Ridanisaurus Rid](https://github.com/Ridanisaurus/), (c) 2013 - 2020
  AlgorithmX2 et al: [CC BY-NC-SA 3.0](https://creativecommons.org/licenses/by-nc-sa/3.0/). Converted to Horizon's
  formats; noncommercial use only.
- Text and translations: [CC0](https://creativecommons.org/publicdomain/zero/1.0/).
- Guidebook click sound, [EminYILDIRIM](https://freesound.org/people/EminYILDIRIM/sounds/536108/):
  [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
