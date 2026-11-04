"""Builds AE2's blocks and items as a Horizon content mod (API 0.2) from the Fabric jar.

Horizon runs no Java, so a block here is a vanilla block's behaviour (its `template`) under AE2's
id, with AE2's models, textures, names, recipes, loot and tags. The jar is for Minecraft 1.20.1;
what Horizon's own carry-over (horizon-port-pack) expects from 1.21 jars is done here too:

- data folders renamed (recipes -> recipe, loot_tables -> loot_table, tags/items -> tags/item...);
- vanilla recipes in 26.3's form (ingredients as ids and #tags, results as {"id", "count"});
  AE2's own recipe types (inscriber, charger...) and Fabric-conditional ones are left out;
- loot tables' silk-touch checks in 26.3's form;
- an `assets/ae2/items/<id>.json` definition for every item;
- each block's blockstate fitted to its template: blocks whose properties no vanilla block has
  (orientation spin, formed multiblocks) keep one look, their default one.

Usage: python make_content_mod.py <jar> <out folder>
Then test with the game argument --dev-mod "<out folder>" (launcher Settings > Game arguments).
"""

import io
import json
import os
import re
import shutil
import sys
import zipfile
from pathlib import Path

from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parent))
from png_tint import FLUIX, compose, read_png, write_png  # noqa: E402

ID = "ae2"

# Blocks whose properties a vanilla block has: the template, and whether the blockstate stays.
TEMPLATE_BY_NAME = [
    (r"_slab$", "minecraft:smooth_quartz_slab", "minecraft:stone_slab"),
    (r"_stairs$", "minecraft:quartz_stairs", "minecraft:stone_stairs"),
    (r"_wall$", "minecraft:cobblestone_wall", "minecraft:cobblestone_wall"),
]
# Blocks with a template of their own: (template, keep the blockstate as it is, properties).
SPECIAL = {
    "quartz_pillar": ("minecraft:quartz_pillar", True, {}),
    "small_quartz_bud": ("minecraft:small_amethyst_bud", True, {}),
    "medium_quartz_bud": ("minecraft:medium_amethyst_bud", True, {}),
    "large_quartz_bud": ("minecraft:large_amethyst_bud", True, {}),
    "quartz_cluster": ("minecraft:amethyst_cluster", True, {}),
    # Budding amethyst would grow amethyst: these stay plain blocks.
    "flawless_budding_quartz": ("minecraft:amethyst_block", False, {}),
    "flawed_budding_quartz": ("minecraft:amethyst_block", False, {}),
    "chipped_budding_quartz": ("minecraft:amethyst_block", False, {}),
    "damaged_budding_quartz": ("minecraft:amethyst_block", False, {}),
    "quartz_block": ("minecraft:amethyst_block", False, {}),
    "fluix_block": ("minecraft:amethyst_block", False, {}),
    "quartz_glass": ("minecraft:glass", False, {}),
    "quartz_vibrant_glass": ("minecraft:glass", False, {"light": 15}),
    # A block entity (its fluid) that draws, see-through: the beacon's.
    "sky_stone_tank": ("minecraft:beacon", True, {"hardness": 2.0, "light": 0}),
    "quartz_fixture": ("minecraft:end_rod", True, {}),
    "light_detector": ("minecraft:shulker_box", True, {"hardness": 0.0}),
    "mysterious_cube": ("minecraft:stone", False, {"hardness": 10.0}),
    "not_so_mysterious_cube": ("minecraft:stone", False, {"hardness": 10.0}),
    # A block entity (it hears redstone), see-through: the beacon's, unlit.
    "tiny_tnt": ("minecraft:beacon", True, {"hardness": 0.0, "light": 0}),
}
STONE = ("sky_stone", "quartz_brick", "chiseled_quartz", "cut_quartz", "smooth_quartz", "quartz_bricks")
# Blocks without an item of their own: the cable bus holds parts, paint and the frame are placed
# by other blocks.
NO_ITEM = {"cable_bus", "paint", "matrix_frame"}
# Variant values that make a block's default look.
DEFAULTS = {"false", "north", "0", "online", "block", "up", "none"}

TOOL_TEMPLATES = {
    "_pickaxe": "pickaxe",
    "_axe": "axe",
    "_shovel": "shovel",
    "_hoe": "hoe",
    "_sword": "sword",
}
SINGLE_STACK = re.compile(
    r"(^|_)(storage_cell|portable|wireless|wrench|memory_card|network_tool|color_applicator|"
    r"matter_cannon|charged_staff|entropy_manipulator|knife|creative_.*_cell|view_cell|"
    r"biometric_card|spatial_cell|_cell_\d+k$)"
)

# Models AE2 builds in Java (a "loader"): a plain model that looks like the block's idle state.
STAND_INS = {
    "block/drive": {"parent": f"{ID}:block/drive/drive_base"},
    # The frame drawn into the glass (a second, overlapping cube shows only its far faces in Horizon).
    "block/quartz_glass": {"parent": "minecraft:block/cube_all", "textures": {"all": f"{ID}:block/hz/quartz_glass"}},
    "block/spatial_pylon": {"parent": "minecraft:block/cube_all", "textures": {"all": f"{ID}:item/spatial_pylon"}},
    "item/memory_card": {
        "parent": "minecraft:item/generated",
        "textures": {"layer0": f"{ID}:item/memory_card_base", "layer1": f"{ID}:item/memory_card_pins"},
    },
    # MeteoriteCompassModel: AE2's 3D base; the needle (meteorite_compass_pointer) is drawn over it, turned, by the
    # native mod (hz_compass_pointer_N).
    "item/meteorite_compass": {"parent": f"{ID}:item/meteorite_compass_base"},
    "item/facade": {"parent": "minecraft:item/generated", "textures": {"layer0": f"{ID}:block/part/cable/glass/transparent"}},
}
# AE2 stitches textures/part/ into the block atlas with assets/minecraft/atlases/blocks.json, which
# a content mod may not ship: the part textures move under textures/block/, which vanilla stitches.
PART_TEXTURES = (f"{ID}:part/", f"{ID}:block/part/")

# Cables are parts in AE2 (drawn by its cable bus). Here each is a pipe block: Horizon's chorus-plant template
# joins it to the same block and to "#ae2:cable_connects" on its six sides, as vanilla's PipeBlock does.
COLORS = [
    "white", "orange", "magenta", "light_blue", "yellow", "lime", "pink", "gray", "light_gray", "cyan",
    "purple", "blue", "brown", "green", "red", "black",
]
# Each cable kind: its texture folder, its core's span, and its item suffix (`<colour>_<suffix>`).
CABLE_KINDS = {
    "glass": ("glass", 6, 10, "glass_cable"),
    "covered": ("covered", 5, 11, "covered_cable"),
    "smart": ("smart", 5, 11, "smart_cable"),
    "covered_dense": ("dense_covered", 3, 13, "covered_dense_cable"),
    "smart_dense": ("dense_smart", 3, 13, "smart_dense_cable"),
}
# Every cable: (kind folder, core span, colour), fluix being AE2's "transparent" colour.
CABLES = {
    f"{color}_{suffix}": (folder, lo, hi, color)
    for folder, lo, hi, suffix in CABLE_KINDS.values()
    for color in ["fluix"] + COLORS
}
DIRECTIONS = ["down", "up", "north", "south", "west", "east"]
# Network devices: a block entity (cells, patterns, the network's view of it) and a menu, so they copy the
# barrel, which has both and faces any of six ways. The native mod gives them their block entities.
DEVICES = {
    "controller", "drive", "chest", "interface", "pattern_provider", "io_port", "molecular_assembler",
    "energy_acceptor", "energy_cell", "dense_energy_cell", "creative_energy_cell", "vibration_chamber",
    "crafting_unit", "crafting_accelerator", "crafting_monitor", "1k_crafting_storage", "4k_crafting_storage",
    "16k_crafting_storage", "64k_crafting_storage", "256k_crafting_storage", "wireless_access_point",
    "quantum_ring", "quantum_link", "spatial_io_port", "spatial_pylon", "spatial_anchor", "cell_workbench",
    "inscriber", "charger", "condenser", "crystal_resonance_generator", "growth_accelerator",
}
# Machines that stand upright in AE2 (they only turn about the vertical): facing up or down keeps them level.
UPRIGHT = {"sky_stone_chest", "smooth_sky_stone_chest", "charger", "inscriber", "cell_workbench", "vibration_chamber", "condenser",
           "molecular_assembler"}
# CrystalResonanceGeneratorBlock.getVoxelShape: a 12-wide column, one pixel short at its front.
CRG_SHAPES = {
    "down": [0.125, 0.0625, 0.125, 0.875, 1.0, 0.875], "up": [0.125, 0.0, 0.125, 0.875, 0.9375, 0.875],
    "north": [0.125, 0.125, 0.0625, 0.875, 0.875, 1.0], "south": [0.125, 0.125, 0.0, 0.875, 0.875, 0.9375],
    "west": [0.0625, 0.125, 0.125, 1.0, 0.875, 0.875], "east": [0.0, 0.125, 0.125, 0.9375, 0.875, 0.875],
}
# Blocks whose own AE2 blockstate (facing and powered) maps straight onto a barrel with a `powered` state.
OWN_FACING = {"growth_accelerator"}
# Budding quartz grows buds: a block entity ticks it, so it copies the barrel too (its look doesn't turn).
BUDDING = {"flawless_budding_quartz", "flawed_budding_quartz", "chipped_budding_quartz", "damaged_budding_quartz"}
# Blocks that look different on a powered network: a `powered` state the native mod sets. Models when off and on
# (None: the device's default look for both).
POWERED_LOOKS = {
    "controller": ("ae2:block/controller/controller_block_offline", "ae2:block/controller/controller_block_online"),
    # AE2's `active` (burning fuel): the native mod sets `powered` from the chamber's burn time.
    "vibration_chamber": ("ae2:block/vibration_chamber", "ae2:block/vibration_chamber_on"),
}
# (Budding quartz and the growth accelerator are handled by BUDDING and OWN_FACING.)
# Devices whose block entity draws (cells in a drive, the charger's crystal) copy the beacon, which has a block
# entity and a menu but doesn't occlude: Horizon culls a block entity's drawing inside an occluding block. The
# beacon has no facing, so they declare one (the native mod sets it as they're placed) and an `update` flag the
# native mod flips to send clients their block entity.
SEE_THROUGH = {"drive", "chest", "charger", "molecular_assembler", "inscriber", "sky_stone_chest", "smooth_sky_stone_chest"}
# AE2's sky stone chests: a chest's shape (AE2's item model, drawn by its TESR in Java) and an inventory, but no
# network: they get block entities, not cable connections.
SKY_CHESTS = {"sky_stone_chest", "smooth_sky_stone_chest"}
# AE2's wooden crank: its base faces away from the block it turns; the handle turns (drawn by the native mod).
SEE_THROUGH.add("crank")
SEE_THROUGH_STATE = {
    "state_properties": {
        "facing": {"type": "enum", "values": ["north", "south", "west", "east", "up", "down"]},
        "update": {"type": "boolean"},
    },
    "default_state": {"facing": "north", "update": "false"},
}
# Model rotations for a barrel's facing, the model's front being north.
FACING_ROTATIONS = {
    "north": {}, "south": {"y": 180}, "west": {"y": 270}, "east": {"y": 90},
    "up": {"x": 270}, "down": {"x": 90},
}
# Mod menu types (data/ae2/horizon/menu/): container slots and synced data slots.
MENUS = {
    "drive": {"slots": 10, "data_slots": 0},
    "chest": {"slots": 1, "data_slots": 0},
    "terminal": {"slots": 6, "data_slots": 4},
    "crafting_terminal": {"slots": 16, "data_slots": 4, "results": [10]},
    "pattern_encoding_terminal": {"slots": 7, "data_slots": 4, "results": [1]},
    "pattern_provider": {"slots": 18, "data_slots": 4},
    "interface": {"slots": 10, "data_slots": 2},
    "io_port": {"slots": 15, "data_slots": 4},
    "io_bus": {"slots": 5, "data_slots": 9},
    "storage_bus": {"slots": 5, "data_slots": 9},
    "vibration_chamber": {"slots": 4, "data_slots": 2},
    "inscriber": {"slots": 8, "data_slots": 3, "results": [3]},
    "molecular_assembler": {"slots": 16, "data_slots": 2},
    "sky_chest": {"slots": 36, "data_slots": 0},
    "level_emitter": {"slots": 2, "data_slots": 5},
    "wireless_access_point": {"slots": 3, "data_slots": 2},
    "info": {"slots": 1, "data_slots": 1},
    "spatial_io_port": {"slots": 2, "data_slots": 4},
    "pattern_access_terminal": {"slots": 1, "data_slots": 4},
    "controller": {"slots": 1, "data_slots": 6},
    "crafting_cpu": {"slots": 1, "data_slots": 8},
    "condenser": {"slots": 3, "data_slots": 4, "results": [1]},
    "cell_workbench": {"slots": 5, "data_slots": 3},
    "energy_level_emitter": {"slots": 1, "data_slots": 2},
    "quantum_link": {"slots": 1, "data_slots": 2},
}
# Parts that sit on a cable in AE2, placed here as blocks facing the player:
# (front texture, side texture, back texture).
# Parts that sit on a cable in AE2, placed here as blocks: AE2's part model on the block's front face, with a
# fluix cable stub behind it. Terminals' screens: (light layer prefix) tinted into one front texture.
PART_BLOCKS = {
    "terminal": ("display_base", "terminal"),
    "crafting_terminal": ("display_base", "crafting_terminal"),
    "pattern_encoding_terminal": ("display_base", "pattern_encoding_terminal"),
    "pattern_access_terminal": ("display_base", "pattern_access_terminal"),
    "import_bus": ("import_bus_base", None),
    "export_bus": ("export_bus_base", None),
    "storage_bus": ("storage_bus_base", None),
    # P2P tunnels: AE2's part models, the attuned type's texture on the front.
    "me_p2p_tunnel": ("p2p/p2p_tunnel_me", None),
    "item_p2p_tunnel": ("p2p/p2p_tunnel_items", None),
    "fluid_p2p_tunnel": ("p2p/p2p_tunnel_fluids", None),
    "redstone_p2p_tunnel": ("p2p/p2p_tunnel_redstone", None),
    "light_p2p_tunnel": ("p2p/p2p_tunnel_light", None),
    "fe_p2p_tunnel": ("p2p/p2p_tunnel_fe", None),
    # The flat versions of AE2's interface, pattern provider and energy acceptor (the native mod runs them as the
    # blocks), and the storage and conversion monitors.
    "cable_interface": ("interface_base", None),
    "cable_pattern_provider": ("pattern_provider_base", None),
    "cable_energy_acceptor": ("energy_acceptor", None),
    "storage_monitor": ("display_base", "storage_monitor"),
    "conversion_monitor": ("display_base", "conversion_monitor"),
    # AE2's illuminated panels (PanelPart, SemiDarkPanelPart, DarkPanelPart): monitor_light in the colour's white,
    # half way to its medium, and its medium variant; lit 15 while powered (a twin block, `hz_<name>_lit`).
    "monitor": ("monitor_base", "panel:bright"),
    "semi_dark_monitor": ("monitor_base", "panel:medium"),
    "dark_monitor": ("monitor_base", "panel:dark"),
    # The cable anchor (a part that holds a cable side and joins nothing), the quartz fiber (power across, no
    # channels) and the toggle buses (their front joins the network while powered, or while not).
    "cable_anchor": ("cable_anchor", None),
    "quartz_fiber": ("quartz_fiber", None),
    "toggle_bus": ("toggle_bus_base", None),
    "inverted_toggle_bus": ("inverted_toggle_bus_base", None),
}
# PanelPart.getBrightnessColor: fluix's white variant, the average of white and medium, the medium variant.
PANEL_TINTS = {"bright": 0xD7BBEC, "medium": ((0xD7 + 0x89) // 2) << 16 | ((0xBB + 0x5C) // 2) << 8 | (0xEC + 0xA8) // 2, "dark": 0x895CA8}
PANELS = ("monitor", "semi_dark_monitor", "dark_monitor")
# AE2's status ring models (part/<name>_off, _on, _has_channel) for each part block.
PART_STATUS = {
    "import_bus": "import_bus", "export_bus": "export_bus", "storage_bus": "storage_bus",
    "terminal": "display_status", "crafting_terminal": "display_status", "pattern_encoding_terminal": "display_status",
    "pattern_access_terminal": "display_status", "storage_monitor": "display_status", "conversion_monitor": "display_status",
    "cable_interface": "interface", "cable_pattern_provider": "interface",
    "toggle_bus": "toggle_bus_status", "inverted_toggle_bus": "toggle_bus_status",
    **{t: "p2p/p2p_tunnel_status" for t in ("me_p2p_tunnel", "item_p2p_tunnel", "fluid_p2p_tunnel", "redstone_p2p_tunnel",
                                           "light_p2p_tunnel", "fe_p2p_tunnel")},
}
OPPOSITE = {"north": "south", "south": "north", "east": "west", "west": "east", "up": "down", "down": "up"}

RENAMED_DIRS = [
    ("recipes/", "recipe/"),
    ("loot_tables/", "loot_table/"),
    ("tags/items/", "tags/item/"),
    ("tags/blocks/", "tags/block/"),
    ("tags/fluids/", "tags/fluid/"),
    ("tags/entity_types/", "tags/entity_type/"),
]


def properties(state):
    out = set()
    for key in state.get("variants", {}):
        out |= {p.split("=")[0] for p in key.split(",") if p}
    for part in state.get("multipart", []):
        when = part.get("when", {})
        for cond in [when] + when.get("OR", []) + when.get("AND", []):
            out |= {k for k in cond if k not in ("OR", "AND")}
    return out


def single_variant(state):
    """The block's default look as a blockstate of one variant."""
    variants = state.get("variants")
    if variants:
        def score(key):
            pairs = [p.split("=") for p in key.split(",") if "=" in p]
            return sum(v in DEFAULTS for _, v in pairs)
        key = max(variants, key=score)
        return {"variants": {"": variants[key]}}
    parts = state.get("multipart", [])
    plain = [p["apply"] for p in parts if "when" not in p] or [p["apply"] for p in parts[:1]]
    # Several unconditional parts: the first is the block's body.
    return {"variants": {"": plain[0] if plain else {"model": f"{ID}:block/{'missing'}"}}}


def block_decl(name, state):
    """(declaration, blockstate to write)."""
    props = {}
    if name in SPECIAL:
        template, keep, props = SPECIAL[name]
        decl = {"template": template, "properties": props} if props else {"template": template}
        if name in ("quartz_fixture", "light_detector"):
            decl["state_properties"] = {"odd": {"type": "boolean"}}
            decl["default_state"] = {"odd": "false"}
        if name == "light_detector":
            # QuartzFixtureBlock.SHAPES: a 0.4 cube pushed 0.3 toward the block it stands on.
            def box(f):
                o = {"down": (0, 0.3, 0), "up": (0, -0.3, 0), "north": (0, 0, 0.3), "south": (0, 0, -0.3), "west": (0.3, 0, 0), "east": (-0.3, 0, 0)}[f]
                return [round(0.3 + o[0], 4), round(0.3 + o[1], 4), round(0.3 + o[2], 4), round(0.7 + o[0], 4), round(0.7 + o[1], 4), round(0.7 + o[2], 4)]
            decl["shapes"] = {"property": "facing", "values": {f: [box(f)] for f in ("north", "south", "west", "east", "up", "down")}}
        return decl, state if keep else single_variant(state)
    for pattern, quartz, stone in TEMPLATE_BY_NAME:
        if re.search(pattern, name):
            template = stone if name.startswith("sky_stone") or name.startswith("smooth_sky_stone") else quartz
            return {"template": template}, state
    if not properties(state):
        stone_like = any(name.startswith(s) for s in STONE) or "quartz" in name
        return {"template": "minecraft:quartz_block" if stone_like else "minecraft:iron_block"}, state
    # A machine: iron's hardness and sounds, one look.
    return {"template": "minecraft:iron_block"}, single_variant(state)


def device_decl(name, state):
    """A network device: the barrel's block entity and menu, its default look turned to its facing."""
    if name in OWN_FACING:
        decl = {"template": "minecraft:barrel", "properties": {"hardness": 2.2, "sounds": "minecraft:iron_block"}, **POWERED_STATE}
        return decl, state
    if name == "crystal_resonance_generator":
        # AE2's own six facings (the crystal points along `facing`) and its CrystalResonanceGeneratorBlock box.
        decl = {"template": "minecraft:barrel", "properties": {"hardness": 2.2, "sounds": "minecraft:glass"},
                "shapes": {"property": "facing", "values": {f: [box] for f, box in CRG_SHAPES.items()}}}
        # AE2 writes facing=up as x 360: a whole turn, said as none.
        return decl, {"variants": {k: {**v, "x": v.get("x", 0) % 360} for k, v in state["variants"].items()}}
    look = single_variant(state)["variants"][""]
    if name == "chest":
        # AE2's chest: its base turned to its facing and the terminal screen in the top's window, lit on a powered
        # network (the lights_off / lights_on parts of its multipart, tinted as AE2 tints them).
        decl = {"template": "minecraft:beacon", "properties": {"hardness": 2.2, "light": 0, "sounds": "minecraft:iron_block"},
                "state_properties": {**SEE_THROUGH_STATE["state_properties"], "powered": {"type": "boolean"}},
                "default_state": {**SEE_THROUGH_STATE["default_state"], "powered": "false"}}
        parts = []
        for facing, rot in FACING_ROTATIONS.items():
            parts.append({"when": {"facing": facing}, "apply": {"model": f"{ID}:block/chest/base", **rot}})
            for on in ("false", "true"):
                lights = f"{ID}:block/hz/chest_lights_{'on' if on == 'true' else 'off'}"
                parts.append({"when": {"facing": facing, "powered": on}, "apply": {"model": lights, **rot}})
        return decl, {"multipart": parts}
    if name in SKY_CHESTS:
        look = {"model": f"{ID}:block/hz/{name}_base"}
    if name == "crank":
        look = {"model": f"{ID}:block/crank_base"}
    decl = {"template": "minecraft:barrel", "properties": {"hardness": 2.2, "sounds": "minecraft:iron_block"}}
    if name in SEE_THROUGH:
        decl = {"template": "minecraft:beacon", "properties": {"hardness": 2.2, "light": 0, "sounds": "minecraft:iron_block"}, **SEE_THROUGH_STATE}
    if name == "molecular_assembler":
        decl["state_properties"] = {**decl["state_properties"], "powered": {"type": "boolean"}}
        decl["default_state"] = {**decl["default_state"], "powered": "false"}
        return decl, facing_variants(look, upright=False, powered=(look, look))
    if name == "pattern_provider":
        # PatternProviderBlock.PUSH_DIRECTION: the plain model, or the oriented one towards the side it pushes.
        sides = {"down": {"x": 180}, "up": {}, "north": {"x": 90}, "south": {"x": 90, "y": 180}, "west": {"x": 90, "y": 270}, "east": {"x": 90, "y": 90}}
        decl["state_properties"] = {**decl.get("state_properties", {}), "push_direction": {"type": "enum", "values": ["all", *sides]}}
        decl["default_state"] = {**decl.get("default_state", {}), "push_direction": "all"}
        variants = {}
        for facing in FACING_ROTATIONS:
            variants[f"facing={facing},push_direction=all"] = {"model": f"{ID}:block/pattern_provider"}
            for side, rot in sides.items():
                variants[f"facing={facing},push_direction={side}"] = {"model": f"{ID}:block/pattern_provider_oriented", **rot}
        return decl, {"variants": variants}
    powered = None
    if name in POWERED_LOOKS:
        decl.update(POWERED_STATE)
        off, on = POWERED_LOOKS[name]
        powered = ({**look, "model": off}, {**look, "model": on})
    if name in CRAFTING_CUBE:
        # AE2's crafting cube: unformed, or formed with a ring where the cluster ends (`side_north`.. say a cluster
        # block is that way), the variant for every neighbour set.
        decl["state_properties"] = {**decl.get("state_properties", {}),
                                    **{f"side_{d}": {"type": "boolean"} for d in CUBE_SIDES}, "structure": {"type": "boolean"}}
        decl["default_state"] = {**decl.get("default_state", {}), **{f"side_{d}": "false" for d in CUBE_SIDES}, "structure": "false"}
        if name == "crafting_monitor":
            # MonitorBakedModel: the unit's chassis with its ring on five sides, the monitor's base on the front. The
            # lights (in the monitor's colour) and the job's item the native mod draws over it.
            decl["state_properties"]["powered"] = {"type": "boolean"}
            decl["default_state"]["powered"] = "false"
            parts = []
            for facing, rot in FACING_ROTATIONS.items():
                parts.append({"when": {"structure": "false", "facing": facing}, "apply": {**look, **rot}})
                for mask in range(64):
                    when = {f"side_{d}": "true" if mask >> i & 1 else "false" for i, d in enumerate(CUBE_SIDES)}
                    parts.append({"when": {**when, "structure": "true", "facing": facing}, "apply": {"model": f"{ID}:block/hz/crafting/monitor_{facing}_{mask}"}})
            return decl, {"multipart": parts}
        parts = [{"when": {"structure": "false"}, "apply": look}]
        for mask in range(64):
            when = {f"side_{d}": "true" if mask >> i & 1 else "false" for i, d in enumerate(CUBE_SIDES)}
            parts.append({"when": {**when, "structure": "true"}, "apply": {"model": f"{ID}:block/hz/crafting/{CRAFTING_CUBE[name]}_{mask}"}})
        light = CRAFTING_LIGHTS.get(name)
        if light:
            decl["state_properties"]["powered"] = {"type": "boolean"}
            decl["default_state"]["powered"] = "false"
            for on in ("false", "true"):
                parts.append({"when": {"structure": "true", "powered": on}, "apply": {"model": f"{ID}:block/hz/crafting/{light}_light_{'on' if on == 'true' else 'off'}"}})
        return decl, {"multipart": parts}
    if name == "wireless_access_point":
        # AE2's own multipart: the chassis, the pearl (off, or lit) and the status lights by `state`, which the
        # native mod sets from the network (off, powered, has a channel).
        decl["state_properties"] = {"state": {"type": "enum", "values": ["off", "on", "has_channel"]}}
        decl["default_state"] = {"state": "off"}
        return decl, state
    if name == "charger":
        # AE2's side ports show only where a cable meets them: `left` (the LeftPort, east when facing north), `right`.
        decl["state_properties"] = {**decl["state_properties"], "left": {"type": "boolean"}, "right": {"type": "boolean"}}
        decl["default_state"] = {**decl["default_state"], "left": "false", "right": "false"}
        variants = {}
        for facing, rot in FACING_ROTATIONS.items():
            rot = {} if facing in ("up", "down") else rot
            for left in ("false", "true"):
                for right in ("false", "true"):
                    look = "body" + ("_left" if left == "true" else "") + ("_right" if right == "true" else "")
                    variants[f"facing={facing},left={left},right={right}"] = {"model": f"{ID}:block/hz/charger_{look}", **rot}
        return decl, {"variants": variants}
    if name in ("energy_cell", "dense_energy_cell"):
        # AE2's charge display: five looks by how full the cell is (`fullness` in AE2; `stage` here, sorting after
        # the barrel's own properties). The native mod sets it from the network's stored power.
        decl["state_properties"] = {**decl.get("state_properties", {}), "stage": {"type": "enum", "values": ["0", "1", "2", "3", "4"]}}
        decl["default_state"] = {**decl.get("default_state", {}), "stage": "0"}
        return decl, {"variants": {f"stage={n}": {"model": f"{ID}:block/{name}_{n}"} for n in range(5)}}
    if name == "controller":
        # AE2's own multipart: block / column / inside looks, online / offline / conflicted (the native mod sets
        # `type` and `state` from the controllers around it and the network).
        decl["state_properties"] = {**decl["state_properties"],
                                    "state": {"type": "enum", "values": ["offline", "online", "conflicted"]},
                                    "type": {"type": "enum", "values": ["block", "column_x", "column_y", "column_z", "inside_a", "inside_b"]}}
        decl["default_state"] = {**decl["default_state"], "state": "offline", "type": "block"}
        return decl, state
    if name == "spatial_pylon":
        # AE2's SpatialPylonBakedModel as states: alone, or a column's start / middle / end on an axis; lit online.
        decl["state_properties"] = {"span": {"type": "enum", "values": PYLON_SPANS}, "state": {"type": "enum", "values": ["offline", "online"]}}
        decl["default_state"] = {"span": "none", "state": "offline"}
        variants = {}
        for span in PYLON_SPANS:
            for st in ("offline", "online"):
                variants[f"span={span},state={st}"] = {"model": f"{ID}:block/hz/pylon/{span}_{st}" if span != "none" else f"{ID}:block/hz/pylon/none"}
        return decl, {"variants": variants}
    if name in ("quantum_ring", "quantum_link"):
        # AE2's QnbFormedBakedModel as states: the chamber, corner and edge rings once formed (lit while powered),
        # glass cable joining the chamber and corners to the ring.
        decl["state_properties"] = {"power": {"type": "enum", "values": ["off", "on"]},
                                    "qnb": {"type": "enum", "values": ["none", "link", "corner", "edge"]},
                                    **{f"side_{d}": {"type": "boolean"} for d in QNB_SIDES}}
        decl["default_state"] = {"power": "off", "qnb": "none", **{f"side_{d}": "false" for d in QNB_SIDES}}
        plain = f"{ID}:block/qnb/{'ring' if name == 'quantum_ring' else 'link'}"
        parts = [{"when": {"qnb": "none"}, "apply": {"model": plain}},
                 {"when": {"qnb": "link"}, "apply": {"model": f"{ID}:block/hz/qnb/link_formed"}}]
        for role in ("corner", "edge"):
            parts.append({"when": {"qnb": role, "power": "off"}, "apply": {"model": f"{ID}:block/hz/qnb/{role}"}})
            parts.append({"when": {"qnb": role, "power": "on"}, "apply": {"model": f"{ID}:block/hz/qnb/{role}_lit"}})
        for d in QNB_SIDES:
            parts.append({"when": {"qnb": "link|corner", f"side_{d}": "true"}, "apply": {"model": f"{ID}:block/hz/qnb/stub_{d}"}})
        return decl, {"multipart": parts}
    return decl, facing_variants(look, upright=name in UPRIGHT, powered=powered)


PYLON_SPANS = ["none"] + [f"{a}_{p}" for a in "xyz" for p in ("start", "middle", "end")]
QNB_SIDES = ("down", "up", "north", "south", "west", "east")


def _inset(elements):
    """Faces lying on the block's edge, pulled a hair inside: Horizon culls an edge face against the block next to
    it (a terminal's back against its cable would vanish)."""
    for e in elements:
        for k in ("from", "to"):
            e[k] = [min(max(v, 0.01), 15.99) for v in e[k]]
    return elements


def _turned(box, facing):
    """A box (0..16) turned as the blockstate turns the model for `facing` (front north), in block units."""
    x1, y1, z1, x2, y2, z2 = box
    turn = {"south": lambda x, y, z: (16 - x, y, 16 - z), "east": lambda x, y, z: (16 - z, y, x),
            "west": lambda x, y, z: (z, y, 16 - x), "down": lambda x, y, z: (x, z, 16 - y),
            "up": lambda x, y, z: (x, 16 - z, y)}.get(facing, lambda x, y, z: (x, y, z))
    pts = [turn(x, y, z) for x in (x1, x2) for y in (y1, y2) for z in (z1, z2)]
    return [round(min(p[i] for p in pts) / 16, 4) for i in range(3)] + [round(max(p[i] for p in pts) / 16, 4) for i in range(3)]


def part_shapes(elements, stub=False):
    """A part's outline and collision: its model's size, not a whole block (AE2's part boxes), for each facing.
    With `stub`, the glass cable from it to its back too (the piece a wired bus, plane or tunnel shows; the
    shape can't follow `wired`, so it is always solid)."""
    if not elements:
        return None
    box = [min(e["from"][i] for e in elements) for i in range(3)] + [max(e["to"][i] for e in elements) for i in range(3)]
    boxes = [box] + ([[6, 6, box[5], 10, 10, 16]] if stub and box[5] < 16 else [])
    return {"property": "facing", "values": {f: [_turned(b, f) for b in boxes] for f in ("north", "south", "west", "east", "up", "down")}}


def _vars(model):
    """Horizon resolves a face's texture only through a `#variable`: each texture a model names gets one."""
    names = {}
    for e in model["elements"]:
        for f in e["faces"].values():
            t = f["texture"]
            if not t.startswith("#"):
                key = names.setdefault(t, f"t{len(names)}")
                f["texture"] = "#" + key
    model["textures"] = {**model.get("textures", {}), **{v: k for k, v in names.items()}}
    return model


def multiblock_models(write):
    """The spatial pylon's and the quantum bridge's formed models (AE2 builds both in code)."""
    tex = lambda n: f"{ID}:block/spatial_pylon/{n}"
    faces = ("down", "up", "north", "south", "west", "east")
    caps = {"x": ("west", "east"), "y": ("down", "up"), "z": ("north", "south")}
    # SpatialPylonBakedModel's UV turns (quarter turns) and flips for each axis and place on the column.
    turns = {"x": {"north": 1, "south": 1, "up": 3, "down": 3}, "y": {}, "z": {"west": 1, "east": 1}}
    flips = {("y", "end"): {"north", "south", "west", "east"},
             ("x", "start"): {"up", "down", "north"}, ("x", "end"): {"south"},
             ("z", "start"): {"up", "east"}, ("z", "end"): {"down", "west"}}

    def face(texture, flip=False, turn=0):
        out = {"texture": texture, "uv": [0, 16, 16, 0] if flip else [0, 0, 16, 16]}
        if turn:
            out["rotation"] = 90 * turn
        return out

    write(f"assets/{ID}/models/block/hz/pylon/none.json", _vars({
        "parent": "block/block", "textures": {"particle": tex("base")},
        "elements": [{"from": [0, 0, 0], "to": [16, 16, 16], "faces": {f: face(tex("base")) for f in faces}},
                     {"from": [0, 0, 0], "to": [16, 16, 16], "faces": {f: face(tex("dim")) for f in faces}}]}))
    for axis in "xyz":
        for place in ("start", "middle", "end"):
            for st in ("offline", "online"):
                light = "dim" if st == "online" else "red"
                outer, inner = {}, {}
                for f in faces:
                    flip = f in flips.get((axis, place), set())
                    turn = turns[axis].get(f, 0)
                    if f in caps[axis]:
                        outer[f] = face(tex("base"))
                        inner[f] = face(tex(light))
                    else:
                        suffix = "_spanned" if place == "middle" else "_end"
                        outer[f] = face(tex("base" + suffix), flip, turn)
                        inner[f] = face(tex(light + suffix), flip, turn)
                write(f"assets/{ID}/models/block/hz/pylon/{axis}_{place}_{st}.json", _vars({
                    "parent": "block/block", "textures": {"particle": tex("base")},
                    "elements": [{"from": [0, 0, 0], "to": [16, 16, 16], "faces": outer},
                                 {"from": [0, 0, 0], "to": [16, 16, 16], "faces": inner}]}))
    # The quantum bridge, formed: a smaller chamber, corners, edges as three bars, lights, glass cable stubs.
    ring, link = f"{ID}:block/quantum_ring", f"{ID}:block/quantum_link"
    cube = lambda a, b, t: {"from": a, "to": b, "faces": {f: {"texture": t} for f in faces}}
    write(f"assets/{ID}/models/block/hz/qnb/link_formed.json",
          _vars({"parent": "block/block", "textures": {"particle": link}, "elements": [cube([2, 2, 2], [14, 14, 14], link)]}))
    corner = [cube([2, 2, 2], [14, 14, 14], ring)]
    edge = [cube([0, 2, 2], [16, 14, 14], ring), cube([2, 0, 2], [14, 16, 14], ring), cube([2, 2, 0], [14, 14, 16], ring)]
    for role, els, light in (("corner", corner, "quantum_ring_light_corner"), ("edge", edge, "quantum_ring_light")):
        write(f"assets/{ID}/models/block/hz/qnb/{role}.json", _vars({"parent": "block/block", "textures": {"particle": ring}, "elements": json.loads(json.dumps(els))}))
        lit = els + [cube([1.99, 1.99, 1.99], [14.01, 14.01, 14.01], f"{ID}:block/{light}")]
        write(f"assets/{ID}/models/block/hz/qnb/{role}_lit.json", _vars({"parent": "block/block", "textures": {"particle": ring}, "elements": json.loads(json.dumps(lit))}))
    glass = f"{ID}:block/part/cable/glass/transparent"
    stubs = {"down": ([6, 0, 6], [10, 2, 10]), "up": ([6, 14, 6], [10, 16, 10]), "north": ([6, 6, 0], [10, 10, 2]),
             "south": ([6, 6, 14], [10, 10, 16]), "west": ([0, 6, 6], [2, 10, 10]), "east": ([14, 6, 6], [16, 10, 10])}
    for d, (a, b) in stubs.items():
        write(f"assets/{ID}/models/block/hz/qnb/stub_{d}.json", _vars({"parent": "block/block", "textures": {"particle": glass}, "elements": [cube(a, b, glass)]}))


CUBE_SIDES = ("north", "south", "east", "west", "up", "down")
# Crafting blocks and their formed look's name; the light over the base, when there is one.
# The formed crafting blocks with AE2's light (crafting/<x>_light, animated for storage): the light texture's name.
CRAFTING_LIGHTS = {"crafting_accelerator": "accelerator", "1k_crafting_storage": "1k_storage", "4k_crafting_storage": "4k_storage",
                   "16k_crafting_storage": "16k_storage", "64k_crafting_storage": "64k_storage", "256k_crafting_storage": "256k_storage"}
CRAFTING_CUBE = {"crafting_unit": "unit", "crafting_accelerator": "accelerator", "crafting_monitor": "monitor",
                 **{f"{k}_crafting_storage": f"{k}_storage" for k in ("1k", "4k", "16k", "64k", "256k")}}
# Each face's edges (top, bottom, left, right) as the neighbour each one borders.
FACE_EDGES = {"north": ("up", "down", "east", "west"), "south": ("up", "down", "west", "east"),
              "east": ("up", "down", "south", "north"), "west": ("up", "down", "north", "south"),
              "up": ("north", "south", "west", "east"), "down": ("south", "north", "west", "east")}


def crafting_cube_models(write, read):
    """AE2's formed crafting cube (CraftingCubeBakedModel): each face's quarters are the base, or a quarter of the
    ring (side or corner) where the face's edge is the cluster's, with the block's light over all of it. A texture
    for each set of ring edges, and a model for each set of neighbours in the cluster."""
    tex = lambda n: read_png(read(f"assets/{ID}/textures/block/crafting/{n}.png"))
    corner, hor, ver = tex("ring_corner"), tex("ring_side_hor"), tex("ring_side_ver")

    def over(dst, src, x0, y0, x1, y1, sy=0):
        for y in range(y0, y1):
            for x in range(x0, x1):
                r, g, b, a = src[2][sy + y][x * 4:x * 4 + 4]
                if a:
                    k = a / 255
                    d = dst[sy + y]
                    d[x * 4:x * 4 + 3] = bytes(round(c * k + o * (1 - k)) for c, o in zip((r, g, b), d[x * 4:x * 4 + 3]))
                    d[x * 4 + 3] = max(d[x * 4 + 3], a)

    for name, look in CRAFTING_CUBE.items():
        if look == "monitor":
            base = tex("monitor_base")
            for edges in range(16):
                rows = [bytearray(base[2][y]) for y in range(16)]
                for qy in (0, 1):
                    for qx in (0, 1):
                        v = edges >> qy & 1
                        h = edges >> (2 + qx) & 1
                        src = corner if v and h else hor if v else ver if h else None
                        if src:
                            over(rows, src, qx * 8, qy * 8, qx * 8 + 8, qy * 8 + 8)
                write(f"assets/{ID}/textures/block/hz/crafting/monitor_{edges}.png", write_png(16, 16, rows))
            for facing in FACE_EDGES:
                for mask in range(64):
                    joined = {d: mask >> i & 1 for i, d in enumerate(CUBE_SIDES)}
                    faces = {}
                    for face, (top, bottom, left, right) in FACE_EDGES.items():
                        edges = sum((not joined[d]) << i for i, d in enumerate((top, bottom, left, right)))
                        faces[face] = f"{ID}:block/hz/crafting/{'monitor' if face == facing else 'unit'}_{edges}"
                    write(f"assets/{ID}/models/block/hz/crafting/monitor_{facing}_{mask}.json",
                          {"parent": "minecraft:block/cube", "textures": {**faces, "particle": faces[facing]}})
            continue
        base = tex("unit_base" if look == "unit" else "light_base")
        light_name = None if look == "unit" else f"{look}_light"
        light = tex(light_name) if light_name else None
        frames = light[1] // 16 if light else 1
        for edges in range(16):  # bits: top, bottom, left, right on the ring
            rows = [bytearray(base[2][y % 16]) for y in range(16 * frames)]
            for f in range(frames):
                for qy in (0, 1):
                    for qx in (0, 1):
                        v = edges >> qy & 1
                        h = edges >> (2 + qx) & 1
                        src = corner if v and h else hor if v else ver if h else None
                        if src:
                            ring = (src[0], src[1], [src[2][y % 16] for y in range(16 * frames)])
                            over(rows, ring, qx * 8, qy * 8, qx * 8 + 8, qy * 8 + 8, f * 16)
                if light:
                    over(rows, light, 0, 0, 16, 16, f * 16)
            write(f"assets/{ID}/textures/block/hz/crafting/{look}_{edges}.png", write_png(16, 16 * frames, rows))
            if light_name and frames > 1:
                write(f"assets/{ID}/textures/block/hz/crafting/{look}_{edges}.png.mcmeta",
                      read(f"assets/{ID}/textures/block/crafting/{light_name}.png.mcmeta"))
        for mask in range(64):
            joined = {d: mask >> i & 1 for i, d in enumerate(CUBE_SIDES)}
            faces = {}
            for face, (top, bottom, left, right) in FACE_EDGES.items():
                edges = sum((not joined[d]) << i for i, d in enumerate((top, bottom, left, right)))
                faces[face] = f"{ID}:block/hz/crafting/{look}_{edges}"
            write(f"assets/{ID}/models/block/hz/crafting/{look}_{mask}.json",
                  {"parent": "minecraft:block/cube", "textures": {**faces, "particle": faces["north"]}})


def charger_models(write, read):
    """The charger split into its body and the two side ports, which its blockstate shows apart."""
    model = json.loads(read(f"assets/{ID}/models/block/charger.json"))
    # A side's port and the strut holding it out past the frame show only with a cable there.
    ports = {"left": ("LeftPort", "LeftStrut"), "right": ("RightPort", "RightStrut")}
    body = [e for e in model["elements"] if not any(e.get("name") in v for v in ports.values())]
    port = {k: [e for e in model["elements"] if e.get("name") in v] for k, v in ports.items()}
    pieces = {"body": body, "body_left": body + port["left"], "body_right": body + port["right"],
              "body_left_right": body + port["left"] + port["right"]}
    for k, elements in pieces.items():
        write(f"assets/{ID}/models/block/hz/charger_{k}.json", {"textures": model["textures"], "elements": elements})


def item_decl(name):
    for suffix, tool in TOOL_TEMPLATES.items():
        if name.endswith(suffix) and not name.endswith("_cell"):
            metal = "diamond" if name.startswith("fluix") else "iron"
            return {"template": f"minecraft:{metal}_{tool}"}
    decl = {"template": "minecraft:stick"}
    if SINGLE_STACK.search(name) and "component" not in name and "housing" not in name:
        decl["max_stack_size"] = 1
    return decl


# Reads a file of the jar, for ingredients that name a tag's contents.
JAR_READ = None


def tag_items(tag):
    ns, path = tag.split(":", 1)
    values = json.loads(JAR_READ(f"data/{ns}/tags/items/{path}.json"))["values"]
    return [x for x in values if not x.startswith("#")]


def difference_items(v):
    def items(i):
        if "tag" in i:
            return tag_items(i["tag"])
        return [i["item"]] if "item" in i else []
    subtracted = v["subtracted"]
    gone = set(sum((items(i) for i in subtracted), [])) if isinstance(subtracted, list) else set(items(subtracted))
    return [x for x in items(v["base"]) if x not in gone] or None


def ingredient(v):
    """A 1.20 ingredient in 26.3's form, or None."""
    if isinstance(v, str):
        return v
    # Fabric's custom ingredient, as the items it leaves: AE2's cable cleaning (a colour's cables, not fluix).
    if isinstance(v, dict) and v.get("fabric:type") == "fabric:difference":
        return difference_items(v)
    if isinstance(v, dict) and len(v) == 1:
        if "item" in v:
            return v["item"]
        if "tag" in v:
            return "#" + v["tag"]
        return None
    if isinstance(v, list):
        out = [ingredient(x) for x in v]
        if any(x is None or x.startswith("#") for x in out):
            return None
        return out[0] if len(out) == 1 else out
    return None


def upgrade_ae2_recipe(r):
    """One of AE2's own recipe types (inscriber, charger...), which the native mod reads: its ingredients and
    result in 26.3's spelling, the rest as AE2 wrote it."""
    def ing(v):
        if isinstance(v, dict) and set(v) <= {"item", "tag", "count"}:
            out = v["item"] if "item" in v else "#" + v["tag"]
            return out
        if isinstance(v, list):
            return [ing(x) for x in v]
        return v
    r = dict(r)
    if isinstance(r.get("ingredients"), dict):
        r["ingredients"] = {k: ing(v) for k, v in r["ingredients"].items()}
    elif "ingredients" in r:
        r["ingredients"] = ing(r["ingredients"])
    if "ingredient" in r:
        r["ingredient"] = ing(r["ingredient"])
    res = r.get("result")
    if isinstance(res, dict) and "item" in res:
        r["result"] = {"id": res["item"], **({"count": res["count"]} if res.get("count", 1) != 1 else {})}
    return r


def upgrade_recipe(r):
    """26.3's form of a vanilla recipe, or None to leave it out."""
    kind = r.get("type", "")
    if "fabric:load_conditions" in r:
        return None
    if kind in ("ae2:inscriber", "ae2:charger", "ae2:transform", "ae2:entropy"):
        return upgrade_ae2_recipe(r)
    if not kind.startswith("minecraft:"):
        return None
    r = dict(r)
    if "key" in r:
        key = {k: ingredient(v) for k, v in r["key"].items()}
        if None in key.values():
            return None
        r["key"] = key
    if "ingredients" in r:
        ings = [ingredient(v) for v in r["ingredients"]]
        if None in ings:
            return None
        r["ingredients"] = ings
    for field in ("ingredient", "base", "addition", "template"):
        if field in r:
            v = ingredient(r[field])
            if v is None:
                return None
            r[field] = v
    result = r.get("result")
    if isinstance(result, dict) and "item" in result:
        result = {"id": result["item"], **({"count": result["count"]} if result.get("count", 1) != 1 else {})}
    elif isinstance(result, str):
        result = {"id": result, **({"count": r.pop("count")} if r.get("count", 1) != 1 else {})}
        r.pop("count", None)
    r["result"] = result
    return r


def upgrade_loot(v):
    """Silk touch checks as 26.3's item predicates, everywhere in a loot table."""
    if isinstance(v, list):
        return [upgrade_loot(x) for x in v]
    if not isinstance(v, dict):
        return v
    out = {k: upgrade_loot(x) for k, x in v.items()}
    if out.get("condition") == "minecraft:match_tool":
        pred = out.get("predicate", {})
        if "enchantments" in pred:
            ench = [
                {"enchantments": e["enchantment"], "levels": e.get("levels", {"min": 1})}
                for e in pred.pop("enchantments")
            ]
            pred.setdefault("predicates", {})["minecraft:enchantments"] = ench
        if "items" in pred and isinstance(pred["items"], list):
            pred["items"] = pred["items"][0] if len(pred["items"]) == 1 else pred["items"]
        out["predicate"] = pred
    return out


def retexture(model):
    """A model with its part textures at their new place."""
    for key, value in model.get("textures", {}).items():
        if isinstance(value, str) and value.startswith(PART_TEXTURES[0]):
            model["textures"][key] = PART_TEXTURES[1] + value[len(PART_TEXTURES[0]):]
    return model


# AE2's cable colours (AEColor's medium and bright variants, its "transparent" being fluix), for channel lights.
CABLE_TINTS = {
    "fluix": (0x8E5ED5, 0xD8B6FF), "white": (0xBEBEBE, 0xFAFAFA), "orange": (0xF99739, 0xFFC58B),
    "magenta": (0xC54AC0, 0xF28FED), "light_blue": (0x6AA4E4, 0xA4D3FF), "yellow": (0xE5D32F, 0xFFF59B),
    "lime": (0x6CCF1D, 0xB7F27B), "pink": (0xF483A9, 0xFFC4D8), "gray": (0x6D6D6D, 0xA8A8A8),
    "light_gray": (0x9D9D9D, 0xD3D3D3), "cyan": (0x1F9AA7, 0x6CE4F0), "purple": (0x8A39C0, 0xC88BF0),
    "blue": (0x3B4CC6, 0x8A97FF), "brown": (0x7C5233, 0xB8885F), "green": (0x4F7A21, 0x8EC055),
    "red": (0xC13A35, 0xF47F7A), "black": (0x2B2B2B, 0x5E5E5E),
}
# Smart cables light up to four pairs of channel lights (AE2's channels_0N and channels_1N), by the channels they
# carry; the native mod sets it (`x_lights`, sorting after the cable's own properties).
SMART_KINDS = {"smart", "dense_smart"}
LIGHTS = 9
STRAIGHT_TESTS = {
    "z": ({"north": "true", "south": "true"}, ["east", "west", "up", "down"]),
    "x": ({"east": "true", "west": "true"}, ["north", "south", "up", "down"]),
    "y": ({"up": "true", "down": "true"}, ["north", "south", "east", "west"]),
}
# Each direction's arm: AE2's connection model points north; these turn it, the texture held to the world (uvlock)
# as AE2's cable builder lays each face's texture out.
ARM_ROTATIONS = {"north": {}, "south": {"y": 180}, "west": {"y": 270}, "east": {"y": 90}, "up": {"x": 270}, "down": {"x": 90}}
AXIS_OF = {"north": "z", "south": "z", "east": "x", "west": "x", "up": "y", "down": "y"}
STRAIGHT_ROTATIONS = {"z": {}, "x": {"y": 90}, "y": {"x": 90}}


def cable_models(write, read, kind, color):
    """AE2's own cable models (models/part/cable/<kind>/): the core, an arm and a straight run, in this colour,
    and for smart cables one of each per number of channel lights."""
    tex = f"{ID}:block/part/cable/{kind}/{'transparent' if color == 'fluix' else color}"
    base = f"assets/{ID}/models/block/hz_cable/{kind}_{color}"
    lights = range(LIGHTS) if kind in SMART_KINDS else [None]
    for part in ("center", "connection", "straight"):
        model = json.loads(read(f"assets/{ID}/models/part/cable/{kind}/{part}.json"))
        for n in lights:
            textures = {"base": tex, "particle": tex}
            if n is not None:
                textures["channels"] = f"{ID}:block/hz_cable/{kind}_{color}_ch{n}"
            elements = [e for e in model["elements"] if n is not None or all(f["texture"] != "#channels" for f in e["faces"].values())]
            # The lights a hair outside the cable they light, so the two never fight for the same pixels.
            elements = json.loads(json.dumps(elements))
            for e in elements:
                if any(f["texture"] == "#channels" for f in e["faces"].values()):
                    e["from"] = [v - 0.01 for v in e["from"]]
                    e["to"] = [v + 0.01 for v in e["to"]]
            suffix = "" if n is None else f"_ch{n}"
            write(f"{base}_{part}{suffix}.json", {"textures": textures, "elements": elements})
    if kind in SMART_KINDS:
        # The lights: the even ones in the cable's medium colour, the odd ones bright (AE2 tints them so).
        medium, bright = CABLE_TINTS[color]
        light = lambda name: read(f"assets/{ID}/textures/part/cable/{kind}/{name}.png")
        for n in lights:
            empty = write_png(64, 64, [bytearray(64 * 4) for _ in range(64)])
            # AE2's CableBuilder: the first four lines from channels_0N, the next four from channels_1N.
            write(f"assets/{ID}/textures/block/hz_cable/{kind}_{color}_ch{n}.png",
                  compose(empty, [(light(f"channels_0{min(n, 4)}"), medium), (light(f"channels_1{max(n - 4, 0)}"), bright)]))


def cable_parts(kind, color, lights):
    """A cable's multipart: a straight run between two opposite arms (no core), else the core and each arm."""
    model = f"{ID}:block/hz_cable/{kind}_{color}"
    suffix = "" if lights is None else f"_ch{lights}"
    lit = {} if lights is None else {"x_lights": str(lights)}

    def straight(axis):
        on, off = STRAIGHT_TESTS[axis]
        return {**on, **{d: "false" for d in off}, **lit}

    def not_straight(axis):
        on, off = STRAIGHT_TESTS[axis]
        return {"OR": [{d: "false"} for d in on] + [{d: "true"} for d in off]}

    parts = []
    for axis, rot in STRAIGHT_ROTATIONS.items():
        parts.append({"when": straight(axis), "apply": {"model": f"{model}_straight{suffix}", **rot, "uvlock": True}})
    core_when = {"AND": [not_straight(a) for a in STRAIGHT_TESTS] + ([lit] if lit else [])}
    parts.append({"when": core_when, "apply": {"model": f"{model}_center{suffix}"}})
    for d, rot in ARM_ROTATIONS.items():
        when = {"AND": [{d: "true"}, not_straight(AXIS_OF[d])] + ([lit] if lit else [])}
        parts.append({"when": when, "apply": {"model": f"{model}_connection{suffix}", **rot, "uvlock": True}})
    return parts


def whole_numbers(value):
    """A loot table with its whole floats (`1.0`) as integers; chances and other fractions stay."""
    if isinstance(value, dict):
        return {k: whole_numbers(v) for k, v in value.items()}
    if isinstance(value, list):
        return [whole_numbers(v) for v in value]
    if isinstance(value, float) and value.is_integer():
        return int(value)
    return value


def modern_loot(value):
    """A 1.20 loot table in 26.3's shape: a condition's and a function's kind under `type`, `functions` as `modifier`,
    a tool predicate's items as one id or `#tag`."""
    if isinstance(value, list):
        return [modern_loot(v) for v in value]
    if not isinstance(value, dict):
        return value
    out = {}
    for k, v in value.items():
        if k == "functions":
            out["modifier"] = modern_loot(v)
        elif k == "function" or (k == "condition" and isinstance(v, str)):
            out["type"] = v
        elif k == "predicate" and isinstance(v, dict):
            pred = modern_loot(v)
            if isinstance(pred.get("items"), list) and len(pred["items"]) == 1:
                pred["items"] = pred["items"][0]
            if "tag" in pred:
                pred["items"] = "#" + pred.pop("tag")
            out[k] = pred
        else:
            out[k] = modern_loot(v)
    return out


def with_strength(name, decl):
    """AEBlocks' strength for the block: hardness and explosion resistance, as the jar's code sets them."""
    props = decl.get("properties", {})
    plain = name.removeprefix("hz_")
    decorative = ("quartz_block", "quartz_bricks", "quartz_brick_", "quartz_pillar", "quartz_slab", "quartz_stairs", "quartz_wall",
                  "cut_quartz_", "smooth_quartz_", "chiseled_quartz_", "fluix_block", "fluix_slab", "fluix_stairs", "fluix_wall")
    if name == "sky_stone_block":
        strength = (50.0, 150.0)
    elif plain.startswith(("sky_stone_", "smooth_sky_stone_")):
        if props.get("hardness") == 0.0:
            return decl
        strength = (5.0, 150.0)
    elif name.endswith("_budding_quartz") or name.startswith(decorative):
        strength = (3.0, 5.0)
    elif name.endswith("_quartz_bud") or name == "quartz_cluster":
        strength = (1.5, 1.5)
    elif name in ("mysterious_cube", "not_so_mysterious_cube"):
        strength = (10.0, 1000.0)
    elif props.get("hardness") in (0.3, 2.2) or name in ("quartz_glass", "quartz_vibrant_glass"):
        # AEBaseBlock.defaultProps: machines, glass, and the cable bus that cables and parts stand for.
        strength = (2.2, 11.0)
    else:
        return decl
    return {**decl, "properties": {**props, "hardness": strength[0], "explosion_resistance": strength[1]}}


def self_drop(name):
    if name in ("energy_cell", "dense_energy_cell"):
        # The native mod drops them, with their charge.
        return {"type": "minecraft:block", "pools": []}
    return {"type": "minecraft:block", "pools": [{"rolls": 1, "entries": [{"type": "minecraft:item", "name": f"{ID}:{name}"}]}]}


# A cable with facades on it (CableBusBlock with a FacadeContainer): colourless twins on the skull's template (a
# block entity, no support needed), sixteen looks to a block. Its joined arms (six bits, DIRECTIONS order) and its
# facades (six more) pick the block and its `rotation`, and so its collision as CableBusContainer builds it: the core
# and arms of its kind's span and a two-pixel plate for each facade. The native mod draws the cable it holds (its
# block entity says which) and the facades; nothing drops by loot (the mod drops the cable and the facades).
TWIN_CORES = [(6, 10), (5, 11), (3, 13)]
TWINS = []


def twin_boxes(lo, hi, index):
    arms, facades = index & 63, index >> 6
    boxes = [[lo, lo, lo, hi, hi, hi]]
    arm = {"down": [lo, 0, lo, hi, lo, hi], "up": [lo, hi, lo, hi, 16, hi], "north": [lo, lo, 0, hi, hi, lo],
           "south": [lo, lo, hi, hi, hi, 16], "west": [0, lo, lo, lo, hi, hi], "east": [hi, lo, lo, 16, hi, hi]}
    plate = {"down": [0, 0, 0, 16, 2, 16], "up": [0, 14, 0, 16, 16, 16], "north": [0, 0, 0, 16, 16, 2],
             "south": [0, 0, 14, 16, 16, 16], "west": [0, 0, 0, 2, 16, 16], "east": [14, 0, 0, 16, 16, 16]}
    for i, d in enumerate(DIRECTIONS):
        if arms >> i & 1:
            boxes.append(arm[d])
        if facades >> i & 1:
            boxes.append(plate[d])
    return [[v / 16 for v in b] for b in boxes]


def add_facade_twins(write, report):
    write(f"assets/{ID}/models/block/hz/empty.json",
          {"textures": {"particle": f"{ID}:block/part/cable/glass/transparent"}, "elements": []})
    for core, (lo, hi) in enumerate(TWIN_CORES):
        for n in range(256):
            name = f"hz_fcb{core}_{n}"
            write(f"data/{ID}/horizon/block/{name}.json", {
                "template": "minecraft:skeleton_skull", "item": False,
                "properties": {"hardness": 0.3, "sounds": "minecraft:glass", "requires_tool": False, "light": 0},
                "shapes": {"property": "rotation", "values": {str(r): twin_boxes(lo, hi, n * 16 + r) for r in range(16)}},
            })
            write(f"assets/{ID}/blockstates/{name}.json", {"multipart": [{"apply": {"model": f"{ID}:block/hz/empty"}}]})
            write(f"data/{ID}/loot_table/blocks/{name}.json", {"type": "minecraft:block", "pools": []})
            report["blocks"][name] = "minecraft:skeleton_skull"
            TWINS.append(name)


def add_cables(write, report, blocks, read):
    """Cables join devices, fluix cables and cables of their own colour (AE2's AEColor.matches)."""
    for name, (kind, lo, hi, color) in CABLES.items():
        cable_models(write, read, kind, color)
        if kind in SMART_KINDS:
            parts = [p for n in range(LIGHTS) for p in cable_parts(kind, color, n)]
        else:
            parts = cable_parts(kind, color, None)
        decl = {
            "template": "minecraft:chorus_plant",
            "connects_to": f"#{ID}:cable_connects_{color}",
            "thickness": float(hi - lo),
            "properties": {"hardness": 0.3, "sounds": "minecraft:glass", "requires_tool": False},
        }
        if kind in SMART_KINDS:
            decl["state_properties"] = {"x_lights": {"type": "enum", "values": [str(n) for n in range(LIGHTS)]}}
            decl["default_state"] = {"x_lights": "0"}
        write(f"data/{ID}/horizon/block/{name}.json", decl)
        write(f"assets/{ID}/blockstates/{name}.json", {"multipart": parts})
        write(f"data/{ID}/loot_table/blocks/{name}.json", self_drop(name))
        report["blocks"][name] = decl["template"]
        blocks.append(name)
        # Older builds' cable with facades (the cable on the beacon's template, its arms and lights as x_ properties):
        # kept only so saved worlds load it; the native mod turns each into the shaped twin on its first tick.
        legacy = f"hz_fcable_{name}"
        props = {f"x_{d}": {"type": "boolean"} for d in ("down", "east", "north", "south", "up", "west")}
        if kind in SMART_KINDS:
            props["x_lights"] = {"type": "enum", "values": [str(n) for n in range(LIGHTS)]}
        props = dict(sorted(props.items()))
        write(f"data/{ID}/horizon/block/{legacy}.json", {
            "template": "minecraft:beacon", "item": False,
            "properties": {"hardness": 0.3, "sounds": "minecraft:glass", "requires_tool": False, "light": 0},
            "state_properties": props,
            "default_state": {k: ("0" if k == "x_lights" else "false") for k in props},
        })
        write(f"assets/{ID}/blockstates/{legacy}.json", {"multipart": [{"apply": {"model": f"{ID}:block/hz/empty"}}]})
        write(f"data/{ID}/loot_table/blocks/{legacy}.json", self_drop(name))
        report["blocks"][legacy] = "minecraft:beacon"


def facing_variants(model_variant, upright=False, powered=None):
    """A barrel's six facings, each the model turned to it; with `powered` (off variant, on variant), one per
    power state too."""
    out = {}
    looks = [("", model_variant)] if powered is None else [(",powered=false", powered[0]), (",powered=true", powered[1])]
    for suffix, look in looks:
        for facing, rot in FACING_ROTATIONS.items():
            if upright and facing in ("up", "down"):
                rot = {}
            v = dict(look)
            for k, val in rot.items():
                v[k] = (v.get(k, 0) + val) % 360
            out[f"facing={facing}{suffix}"] = v
    return {"variants": out}


POWERED_STATE = {"state_properties": {"powered": {"type": "boolean"}}, "default_state": {"powered": "false"}}


def part_elements(read, name):
    """A part model's elements and textures, through its parents, as AE2's jar has them."""
    elements, textures = None, {}
    while name:
        ns, path = name.split(":", 1) if ":" in name else ("minecraft", name)
        if ns != ID:
            break
        model = json.loads(read(f"assets/{ID}/models/{path}.json"))
        textures = {**model.get("textures", {}), **textures}
        if elements is None and "elements" in model:
            elements = model["elements"]
        name = model.get("parent")
    return elements or [], textures


# Vanilla's blocks and models as Horizon exported them (the facade table needs their face textures).
HORIZON_CACHE = Path(os.environ.get("APPDATA", "")) / "horizon" / ".horizon-cache" / "mod-content"
# AE2's own full blocks a facade can be made of (AEDecorativeBlock and the quartz blocks).
AE2_FACADE_BLOCKS = ["sky_stone_block", "smooth_sky_stone_block", "sky_stone_brick", "sky_stone_small_brick", "fluix_block",
                     "quartz_block", "cut_quartz_block", "smooth_quartz_block", "quartz_bricks", "quartz_pillar",
                     "chiseled_quartz_block", "quartz_glass", "quartz_vibrant_glass"]
FACE_ORDER = ("down", "up", "north", "south", "west", "east")


def facades(write, read, has):
    """FacadeItem.createFacadeForItem's blocks (full cubes drawn by their model, no block entity) and FacadeRecipe
    (four cable anchors round the block, four facades): a recipe for each, the facade's item model (a plate of
    the block's faces, as FacadeBakedItemModel draws it) and the faces the native mod draws a placed facade with."""
    behaviours = HORIZON_CACHE / "block-behaviours.json"
    if not behaviours.exists():
        print("facades: no Horizon cache, none made")
        return
    blocks = json.loads(behaviours.read_text(encoding="utf-8"))["blocks"]

    def model_json(ns, path):
        if ns == ID:
            n = f"assets/{ID}/models/{path}.json"
            return json.loads(read(n)) if n in has else None
        f = HORIZON_CACHE / "assets" / ns / "models" / f"{path}.json"
        return json.loads(f.read_text(encoding="utf-8")) if f.exists() else None

    def resolve(ref):
        ns, path = ref.split(":", 1) if ":" in ref else ("minecraft", ref)
        textures, elements = {}, None
        while True:
            m = model_json(ns, path)
            if m is None:
                return None, None
            for k, v in m.get("textures", {}).items():
                textures.setdefault(k, v)
            if elements is None and "elements" in m:
                elements = m["elements"]
            parent = m.get("parent")
            if not parent:
                break
            ns, path = parent.split(":", 1) if ":" in parent else ("minecraft", parent)
        return textures, elements

    def texture(textures, ref, depth=0):
        plain = lambda r: r.get("sprite", "") if isinstance(r, dict) else (r or "")
        ref = plain(ref)
        while ref.startswith("#") and depth < 10:
            ref = plain(textures.get(ref[1:], ""))
            depth += 1
        if not ref or ref.startswith("#"):
            return None
        return ref if ":" in ref else f"minecraft:{ref}"

    def block_model(ns, name):
        if ns == ID:
            n = f"assets/{ID}/blockstates/{name}.json"
            state = json.loads(read(n)) if n in has else None
        else:
            f = HORIZON_CACHE / "assets" / ns / "blockstates" / f"{name}.json"
            state = json.loads(f.read_text(encoding="utf-8")) if f.exists() else None
        if not state or "variants" not in state:
            return None
        variants = state["variants"]
        v = variants.get("") or variants.get("axis=y") or variants.get("facing=north") or next(iter(variants.values()))
        if isinstance(v, list):
            v = v[0]
        return v.get("model")

    candidates = [(b["name"].split(":", 1)) for b in blocks
                  if "EntityBlock" not in b.get("interfaces", []) and b.get("item") == b["name"] and b["name"] != "minecraft:air"]
    candidates += [[ID, n] for n in AE2_FACADE_BLOCKS]
    table = {}
    for ns, name in candidates:
        ref = block_model(ns, name)
        if not ref:
            continue
        textures, elements = resolve(ref)
        if not elements or len(elements) != 1:
            continue
        el = elements[0]
        if el.get("from") != [0, 0, 0] or el.get("to") != [16, 16, 16] or len(el.get("faces", {})) != 6:
            continue
        faces = [texture(textures, el["faces"][f]["texture"]) for f in FACE_ORDER]
        if not all(faces):
            continue
        table[f"{ns}:{name}"] = faces
    for block, faces in sorted(table.items()):
        ns, name = block.split(":", 1)
        flat = name if ns == "minecraft" else f"{ns}_{name}"
        tex = dict(zip(FACE_ORDER, faces))
        write(f"assets/{ID}/models/item/hz/facade/{flat}.json", {
            "parent": "minecraft:block/block",
            "textures": {**tex, "particle": tex["north"]},
            # FacadeBuilder's plate: two pixels thick.
            "elements": [{"from": [0, 0, 7], "to": [16, 16, 9],
                          "faces": {f: {"texture": f"#{f}", **({"uv": [0, 7, 16, 9]} if f in ("up", "down") else {"uv": [7, 0, 9, 16]} if f in ("west", "east") else {})}
                                    for f in FACE_ORDER}}],
        })
        write(f"assets/{ID}/items/hz_facade_{flat}.json", {"model": {"type": "minecraft:model", "model": f"{ID}:item/hz/facade/{flat}"}})
        write(f"data/{ID}/recipe/hz_facade/{flat}.json", {
            "type": "minecraft:crafting_shaped",
            "category": "misc",
            "pattern": [" A ", "ABA", " A "],
            "key": {"A": "ae2:cable_anchor", "B": block},
            "result": {"id": "ae2:facade", "count": 4,
                       "components": {"minecraft:custom_data": {"item": block}, "minecraft:item_model": f"{ID}:hz_facade_{flat}"}},
        })
    write(f"data/{ID}/hz/facades.json", table)
    print(f"facades: {len(table)} blocks")


def compass_pointers(write, read):
    """MeteoriteCompassBakedModel turns the pointer about the model's centre (rotationY). Sixteen pointer-only models,
    a 22.5-degree step apart (a quarter turn of the box, then an element rotation of -22.5 to 45), each on the base's
    display transforms, and an item model to draw each by."""
    pointer = json.loads(read(f"assets/{ID}/models/item/meteorite_compass_pointer.json"))["elements"][0]

    def quarter(p, q):
        x, y, z = p
        for _ in range(q % 4):
            # rotationY(90 degrees) about (8, 8): (dx, dz) -> (dz, -dx).
            x, z = 8 + (z - 8), 8 - (x - 8)
        return [x, y, z]

    for i in range(16):
        q, k = divmod(i, 4)
        angle = k * 22.5
        if k == 3:
            q, angle = q + 1, -22.5
        a, b = quarter(pointer["from"], q), quarter(pointer["to"], q)
        el = dict(pointer)
        el["from"] = [min(a[j], b[j]) for j in range(3)]
        el["to"] = [max(a[j], b[j]) for j in range(3)]
        if angle:
            el["rotation"] = {"origin": [8, 8, 8], "axis": "y", "angle": angle}
        write(f"assets/{ID}/models/item/hz/compass_pointer_{i}.json", {"parent": f"{ID}:item/meteorite_compass_base", "elements": [el]})
        # The whole compass with the needle so turned: the item's own model while it points (hand, frame, GUI).
        base = json.loads(read(f"assets/{ID}/models/item/meteorite_compass_base.json"))
        write(f"assets/{ID}/models/item/hz/compass_full_{i}.json", {"parent": f"{ID}:item/meteorite_compass_base", "elements": base["elements"] + [el]})
        write(f"assets/{ID}/items/hz_compass_full_{i}.json", {"model": {"type": "minecraft:model", "model": f"{ID}:item/hz/compass_full_{i}"}})
        write(f"assets/{ID}/items/hz_compass_pointer_{i}.json", {"model": {"type": "minecraft:model", "model": f"{ID}:item/hz/compass_pointer_{i}"}})


def add_chest_lights(write, read):
    """The ME chest's screen lights (AE2's chest/lights_off and lights_on): one texture each, AE2's tints baked in
    (Horizon's content models carry no tint), on the top face inside the window."""
    tex = lambda n: read(f"assets/{ID}/textures/block/chest/{n}.png")
    clear = write_png(16, 16, [bytes(64)] * 16)
    looks = {
        "off": compose(clear, [(tex("lights_off"), FLUIX["bright"])]),
        "on": compose(clear, [(tex(f"lights_on_{shade}"), FLUIX[shade]) for shade in ("dark", "medium", "bright")]),
    }
    for state, png in looks.items():
        write(f"assets/{ID}/textures/block/hz/chest_lights_{state}.png", png)
        write(f"assets/{ID}/models/block/hz/chest_lights_{state}.json", {
            "textures": {"lights": f"{ID}:block/hz/chest_lights_{state}"},
            "elements": [{"from": [0, 0, 0], "to": [16, 16, 16], "faces": {"up": {"texture": "#lights"}}}],
        })


def add_part_blocks(write, report, blocks, read):
    """Parts placed as blocks: the part against the block's back face, its front facing the player."""
    for name, (base, lights) in PART_BLOCKS.items():
        elements, textures = part_elements(read, f"{ID}:part/{base}")
        if name == "quartz_fiber":
            # The fiber runs through its block, from the cable behind to the cable in front.
            elements = [{**e, "from": [6, 6, 0], "to": [10, 10, 16]} for e in elements[:1]]
        if lights and lights.startswith("panel:"):
            tex = lambda n: read(f"assets/{ID}/textures/part/{n}.png")
            front = compose(tex("monitor_front"), [(tex("monitor_light"), PANEL_TINTS[lights[6:]])])
            write(f"assets/{ID}/textures/block/hz/{name}_front.png", front)
            textures["front"] = f"{ID}:block/hz/{name}_front"
        elif lights:
            tex = lambda n: read(f"assets/{ID}/textures/part/{n}.png")
            front = compose(
                tex("monitor_front"),
                [(tex(f"{lights}_{shade}"), FLUIX[shade]) for shade in ("dark", "medium", "bright")],
            )
            write(f"assets/{ID}/textures/block/hz/{name}_front.png", front)
            textures["front"] = f"{ID}:block/hz/{name}_front"
        textures = {k: (PART_TEXTURES[1] + v[len(PART_TEXTURES[0]):] if v.startswith(PART_TEXTURES[0]) else v) for k, v in textures.items()}
        depth = max((e["to"][2] for e in elements), default=2)
        # Buses and tunnels sit against the block they work on (the front); terminals against the back.
        bus = name.endswith(("_bus", "_p2p_tunnel"))
        moved = json.loads(json.dumps(elements))
        shift = 0 if bus else 16 - depth
        for e in moved:
            e["from"][2] += shift
            e["to"][2] += shift
            if "rotation" in e and "origin" in e["rotation"]:
                e["rotation"]["origin"][2] += shift
            for face in e.get("faces", {}).values():
                face.pop("cullface", None)
                face.pop("tintindex", None)
        _inset(moved)
        # The item shows the part's front, a little turned, as AE2's part items do.
        display = {
            "gui": {"rotation": [30, 160, 0], "translation": [0, 0, 0], "scale": [0.625, 0.625, 0.625]},
            "ground": {"rotation": [0, 0, 0], "translation": [0, 3, 0], "scale": [0.25, 0.25, 0.25]},
            "fixed": {"rotation": [0, 180, 0], "translation": [0, 0, 0], "scale": [0.5, 0.5, 0.5]},
            "thirdperson_righthand": {"rotation": [75, 225, 0], "translation": [0, 2.5, 0], "scale": [0.375, 0.375, 0.375]},
            "firstperson_righthand": {"rotation": [0, 225, 0], "translation": [0, 0, 0], "scale": [0.4, 0.4, 0.4]},
        }
        lit = moved
        if lights:
            # Powered, AE2's screen lights are `unlit` (they glow): the front face moves to an element of its own
            # with vanilla's light_emission; unpowered (`_off`) the same screen is lit by the world.
            lit = []
            for e in moved:
                front = {k: v for k, v in e["faces"].items() if v.get("texture") == "#front"}
                rest = {k: v for k, v in e["faces"].items() if v.get("texture") != "#front"}
                if front:
                    lit.append({**e, "faces": front, "light_emission": 15})
                if rest:
                    lit.append({**e, "faces": rest})
        write(f"assets/{ID}/models/block/hz_part/{name}.json", {"textures": textures, "elements": lit, "display": display})
        # The item: the part centred in its box, with a block item's usual angles (the front shown, turned).
        centre = 8 - (min((e["from"][2] for e in elements), default=0) + depth) / 2
        centred = json.loads(json.dumps(elements))
        for e in centred:
            e["from"][2] += centre
            e["to"][2] += centre
            if "rotation" in e and "origin" in e["rotation"]:
                e["rotation"]["origin"][2] += centre
            for face in e.get("faces", {}).values():
                face.pop("cullface", None)
                face.pop("tintindex", None)
        write(f"assets/{ID}/models/block/hz_part/{name}_item.json",
              {"parent": "minecraft:block/block", "textures": textures, "elements": centred})
        if lights:
            # Unpowered: AE2's display_off, the same tinted screen without the glow.
            write(f"assets/{ID}/models/block/hz_part/{name}_off.json", {"textures": textures, "elements": moved, "display": display})
        # Glass's hardness: a part comes off a cable at once.
        decl = {"template": "minecraft:barrel", "properties": {"hardness": 0.3, "requires_tool": False, "sounds": "minecraft:iron_block"},
                **POWERED_STATE}
        if part_shapes(moved):
            decl["shapes"] = part_shapes(moved, stub=bus)
        status = PART_STATUS.get(name)
        if status:
            # AE2's three ring states: off, on (powered, no channel), has_channel.
            # (Named to sort after `powered`: Horizon's state table wants a block's properties in name order.)
            decl = {**decl, "state_properties": {"powered": {"type": "boolean"}, "with_channel": {"type": "boolean"}},
                    "default_state": {"powered": "false", "with_channel": "false"}}
        write(f"data/{ID}/horizon/block/{name}.json", decl)
        # The look: the part (its screen lit or not), and AE2's status ring for its state (unpowered off, powered
        # without a channel on, with one has_channel), moved as the part is.
        looks = {"false": f"{ID}:block/hz_part/{name}_off" if lights else f"{ID}:block/hz_part/{name}",
                 "true": f"{ID}:block/hz_part/{name}"}
        if status:
            for which in ("off", "on", "has_channel"):
                ring = json.loads(read(f"assets/{ID}/models/part/{status}_{which}.json"))
                ring_els = json.loads(json.dumps(ring["elements"]))
                for e in ring_els:
                    e["from"][2] += shift
                    e["to"][2] += shift
                _inset(ring_els)
                rtex = {k: (PART_TEXTURES[1] + v[len(PART_TEXTURES[0]):] if v.startswith(PART_TEXTURES[0]) else v) for k, v in ring["textures"].items()}
                write(f"assets/{ID}/models/block/hz_part/{name}_status_{which}.json", {"textures": rtex, "elements": ring_els})
        parts = []
        for facing, rot in FACING_ROTATIONS.items():
            for on in ("false", "true"):
                parts.append({"when": {"facing": facing, "powered": on}, "apply": {"model": looks[on], **rot}})
                if status and on == "false":
                    parts.append({"when": {"facing": facing, "powered": "false"}, "apply": {"model": f"{ID}:block/hz_part/{name}_status_off", **rot}})
                elif status:
                    for ch, which in (("false", "on"), ("true", "has_channel")):
                        parts.append({"when": {"facing": facing, "powered": "true", "with_channel": ch},
                                      "apply": {"model": f"{ID}:block/hz_part/{name}_status_{which}", **rot}})
        write(f"assets/{ID}/blockstates/{name}.json", {"multipart": parts})
        write(f"data/{ID}/loot_table/blocks/{name}.json", self_drop(name))
        if name in PANELS:
            lit = f"hz_{name}_lit"
            write(f"data/{ID}/horizon/block/{lit}.json", {**decl, "item": False, "properties": {**decl["properties"], "light": 15}})
            write(f"assets/{ID}/blockstates/{lit}.json", {"multipart": parts})
            write(f"data/{ID}/loot_table/blocks/{lit}.json", self_drop(name))
            blocks.append(lit)
        # The item shows the part as placed (AE2's item model is tinted in code).
        write(f"assets/{ID}/items/{name}.json", {"model": {"type": "minecraft:model", "model": f"{ID}:block/hz_part/{name}_item"}})
        report["blocks"][name] = decl["template"]
        blocks.append(name)


def add_registries(write, blocks, items):
    """The cable tag, the menus and AE2's creative tab."""
    devices = sorted(b for b in blocks if b in DEVICES or b in PART_BLOCKS)
    for color in ["fluix"] + COLORS:
        cables = sorted(c for c, v in CABLES.items() if color == "fluix" or v[3] in (color, "fluix"))
        write(
            f"data/{ID}/tags/block/cable_connects_{color}.json",
            {"values": [f"{ID}:{b}" for b in devices + cables + TWINS]},
        )
    # DynamicLadderMixin: a cable anchor can be climbed.
    write("data/minecraft/tags/block/climbable.json", {"replace": False, "values": [f"{ID}:cable_anchor"]})
    for name, menu in MENUS.items():
        write(f"data/{ID}/horizon/menu/{name}.json", menu)
    listed = [f"{ID}:{b}" for b in blocks if b not in NO_ITEM and not b.startswith("hz_")] + [f"{ID}:{i}" for i in items if i not in blocks]
    write(
        f"data/{ID}/horizon/creative_tab/main.json",
        {"title": "Applied Energistics 2", "icon": f"{ID}:controller", "items": listed},
    )


def fix_occlusion(out):
    """A block whose model isn't a whole cube must not hide its neighbours' faces (a barrel or stone template
    would: the block below shows a hole). Barrel-based devices move to the beacon template (no occlusion, still a
    block entity and a menu) with their facing as a state of their own; plain blocks move to glass."""
    out = Path(out)
    models_root = out / f"assets/{ID}/models"

    def model(name):
        ns, path = name.split(":", 1) if ":" in name else ("minecraft", name)
        p = models_root / f"{path}.json"
        return json.loads(p.read_text()) if ns == ID and p.exists() else None

    cubes = {"block/cube_all", "block/cube", "block/cube_column", "block/orientable", "block/cube_bottom_top", "block/orientable_vertical"}

    def full(name, depth=0):
        m = model(name)
        if m is None or depth > 6:
            return True
        if m.get("parent", "").replace("minecraft:", "") in cubes and "elements" not in m:
            return True
        els = m.get("elements")
        if els is None:
            return full(m["parent"], depth + 1) if "parent" in m else True
        return any(e["from"] == [0, 0, 0] and e["to"] == [16, 16, 16] for e in els)

    keep = ("beacon", "chorus_plant", "glass", "slab", "stairs", "wall", "fence", "bud", "cluster", "button", "pressure", "torch", "pane", "end_rod")
    changed = []
    for f in sorted((out / f"data/{ID}/horizon/block").glob("*.json")):
        decl = json.loads(f.read_text())
        template = decl.get("template", "")
        if any(k in template for k in keep):
            continue
        # A block with shapes of its own (parts, the crystal resonance generator): its shapes follow the
        # template's `facing`, but a barrel still hides its neighbours' whole faces (the floor under a panel showed
        # a hole). The shulker box has the six facings, a block entity and a menu, and occludes nothing.
        if "shapes" in decl:
            if template == "minecraft:barrel":
                decl["template"] = "minecraft:shulker_box"
                f.write_text(json.dumps(decl, indent=2))
                changed.append(f.stem)
            continue
        bs = out / f"assets/{ID}/blockstates/{f.stem}.json"
        if not bs.exists():
            continue
        state = json.loads(bs.read_text())
        models = set()
        for v in (state.get("variants") or {}).values():
            models.update(x["model"] for x in (v if isinstance(v, list) else [v]))
        for part in state.get("multipart", []):
            a = part["apply"]
            models.update(x["model"] for x in (a if isinstance(a, list) else [a]))
        if not models or any(full(m) for m in models):
            continue
        props = dict(decl.get("properties", {}))
        if template == "minecraft:barrel":
            props.setdefault("light", 0)
            # `update` as the other see-through devices have it: the native mod flips it to redraw the block.
            states = {"facing": {"type": "enum", "values": ["north", "south", "west", "east", "up", "down"]},
                      "update": {"type": "boolean"}, **decl.get("state_properties", {})}
            defaults = {"facing": "north", "update": "false", **decl.get("default_state", {})}
            decl = {**decl, "template": "minecraft:beacon", "properties": props,
                    "state_properties": dict(sorted(states.items())), "default_state": dict(sorted(defaults.items()))}
        else:
            decl = {**decl, "template": "minecraft:glass", "properties": props}
        f.write_text(json.dumps(decl, indent=2))
        changed.append(f.stem)
    return changed


def merge_convention_tags(jar, out):
    """Fabric API's convention tags (fabric-convention-tags-v2, nested in the fabric-api jar beside AE2's): the
    `c:` tags AE2's recipes and P2P attunements name, with their vanilla members, merged with AE2's own."""
    import io
    fabric = Path(jar).resolve().parent / "fabric-api-0.165.0+26.3.jar"
    if not fabric.exists():
        return
    fz = zipfile.ZipFile(fabric)
    for nested in [n for n in fz.namelist() if n.startswith("META-INF/jars/fabric-convention-tags-v2")]:
        cz = zipfile.ZipFile(io.BytesIO(fz.read(nested)))
        for n in cz.namelist():
            if not (n.startswith("data/c/tags/") and n.endswith(".json")):
                continue
            tag = json.loads(cz.read(n))
            p = Path(out) / n
            if p.exists():
                ours = json.loads(p.read_text())
                values = ours.get("values", []) + [v for v in tag.get("values", []) if v not in ours.get("values", [])]
                tag = {**ours, "values": values}
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(json.dumps(tag, indent=2))
    # The older names AE2's recipes use (`c:black_dyes`) as aliases of the current ones (`c:dyes/black`).
    for color in COLORS:
        p = Path(out) / f"data/c/tags/item/{color}_dyes.json"
        if not p.exists():
            p.write_text(json.dumps({"replace": False, "values": [f"#c:dyes/{color}"]}, indent=2))
    # EnchantmentHooks.isDiggerEnchantable: the annihilation plane takes the digger enchantments (efficiency, silk
    # touch, fortune) and, as the anvil sees it damageable, unbreaking. Its enchantability (10) is the native mod's
    # default component.
    # (Its table offers unbreaking too: AE2 counts it damageable only in the anvil, and 26.3's table reads
    # unbreaking's supported items.)
    for tag in ("mining", "mining_loot", "durability"):
        p = Path(out) / f"data/minecraft/tags/item/enchantable/{tag}.json"
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(json.dumps({"replace": False, "values": [f"{ID}:annihilation_plane"]}, indent=2))
    # The convention v1 biome tags AE2's meteorites read (`#ae2:has_meteorites` holds `#c:in_overworld`; the crater
    # and fallout read the others) as aliases of the v2 ones.
    for old, new in [("in_overworld", "is_overworld"), ("ocean", "is_ocean"), ("desert", "is_desert"),
                     ("beach", "is_beach"), ("mesa", "is_badlands"), ("snowy", "is_snowy"), ("icy", "is_icy"),
                     ("plains", "is_plains"), ("forest", "is_forest")]:
        p = Path(out) / f"data/c/tags/worldgen/biome/{old}.json"
        if not p.exists():
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(json.dumps({"replace": False, "values": [{"id": f"#c:{new}", "required": False}]}, indent=2))


def main(jar, out):
    out = Path(out)
    if out.exists():
        shutil.rmtree(out)
    z = zipfile.ZipFile(jar)
    names = [n for n in z.namelist() if not n.endswith("/")]
    read = lambda n: z.read(n)
    lang = json.loads(read(f"assets/{ID}/lang/en_us.json"))
    has = set(names)
    report = {"blocks": {}, "items": 0, "recipes": 0, "recipes_left_out": [], "files": 0}
    global JAR_READ
    JAR_READ = read

    def write(path, data):
        if path.startswith(f"data/{ID}/horizon/block/") and isinstance(data, dict):
            data = with_strength(path.rsplit("/", 1)[1][:-5], data)
        if path.endswith(("/blocks/energy_cell.json", "/blocks/dense_energy_cell.json")):
            # The native mod drops them, with their charge.
            data = {"type": "minecraft:block", "pools": []}
        elif "/loot_table/" in path:
            # The jar's tables say `rolls: 1.0`; Horizon reads rolls and counts as integers.
            data = modern_loot(whole_numbers(json.loads(data) if isinstance(data, bytes) else data))
        p = out / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data if isinstance(data, bytes) else json.dumps(data, indent=2).encode())
        report["files"] += 1

    # Assets: models, textures, names, sounds. AE2's guide, screens and atlas are its code's.
    for n in names:
        if not n.startswith(f"assets/{ID}/"):
            continue
        rest = n[len(f"assets/{ID}/"):]
        if rest.startswith(("ae2guide/", "screens/", "blockstates/")) or rest == "logo.png":
            continue
        if not re.fullmatch(r"[a-z0-9_./-]+", n):
            continue
        if rest.startswith("textures/guis/"):
            write(f"assets/{ID}/textures/gui/{rest[len('textures/guis/'):]}", read(n))
            continue
        if rest.startswith("textures/part/"):
            write(f"assets/{ID}/textures/block/{rest[len('textures/'):]}", read(n))
            continue
        if rest.startswith("models/") and n.endswith(".json"):
            model = json.loads(read(n))
            if "loader" in model:
                stand_in = STAND_INS.get(rest[len("models/"):-len(".json")])
                if stand_in is None:
                    # A part or formed-multiblock model: nothing places it on its own.
                    continue
                model = json.loads(json.dumps(stand_in))
            write(n, retexture(model))
            continue
        write(n, read(n))

    # Blocks.
    blocks = []
    for n in names:
        m = re.fullmatch(rf"assets/{ID}/blockstates/([a-z0-9_]+)\.json", n)
        if not m or m.group(1).startswith("debug_"):
            continue
        name = m.group(1)
        state = json.loads(read(n))
        if name in BUDDING:
            look = single_variant(state)["variants"][""]
            decl = {"template": "minecraft:barrel", "properties": {"hardness": 1.5, "sounds": "minecraft:amethyst_block"}}
            state = facing_variants(look, upright=True)
            state = {"variants": {k: look for k in state["variants"]}}
        else:
            decl, state = device_decl(name, state) if name in DEVICES or name in SKY_CHESTS or name == "crank" else block_decl(name, state)
        if name in NO_ITEM:
            decl["item"] = False
        write(f"data/{ID}/horizon/block/{name}.json", decl)
        write(f"assets/{ID}/blockstates/{name}.json", state)
        report["blocks"][name] = decl["template"]
        blocks.append(name)

    add_cables(write, report, blocks, read)
    add_facade_twins(write, report)
    add_part_blocks(write, report, blocks, read)
    add_chest_lights(write, read)
    # PaintSplotchesBlock: no collision or outline, light 0, 12 or 15 for its lumen splotches (a twin each), a block
    # entity holding the splotches; nothing drops.
    for name, light in (("paint", 0), ("hz_paint_lit1", 12), ("hz_paint_lit2", 15)):
        # The skull's template: a block entity, needs no support, and its own `rotation` keys the (empty) shapes.
        write(f"data/{ID}/horizon/block/{name}.json", {
            "template": "minecraft:skeleton_skull", "item": False,
            "properties": {"hardness": 0.0, "light": light, "sounds": "minecraft:slime_block"},
            "shapes": {"property": "rotation", "values": {str(r): [] for r in range(16)}},
        })
        write(f"assets/{ID}/blockstates/{name}.json", {"multipart": [{"apply": {"model": f"{ID}:block/paint"}}]})
        write(f"data/{ID}/loot_table/blocks/{name}.json", {"type": "minecraft:block", "pools": []})
        if name != "paint":
            blocks.append(name)
    # The formed crafting lights: the light texture on every face over the cube (culled against the cluster), the
    # powered one glowing (LightBakedModel's emissive material).
    for light in CRAFTING_LIGHTS.values():
        for state, glow in (("off", 0), ("on", 15)):
            el = {"from": [0, 0, 0], "to": [16, 16, 16],
                  "faces": {d: {"texture": "#light", "cullface": d} for d in ("north", "south", "east", "west", "up", "down")}}
            if glow:
                el["light_emission"] = glow
            write(f"assets/{ID}/models/block/hz/crafting/{light}_light_{state}.json",
                  {"textures": {"light": f"{ID}:block/crafting/{light}_light", "particle": f"{ID}:block/crafting/{light}_light"}, "elements": [el]})
    # The storage monitors' amount font (a GUI texture: the native mod draws it on the screen).
    from monitor_font import sheet
    write(f"assets/{ID}/textures/gui/hz/monitor_font.png", write_png(256, 8, sheet()))
    # What the native mod draws on blocks with the GUI atlas: the monitor's lights, the assembler's powered light
    # and its crafting sparks (CraftingFx's sprite).
    for light in ("bright", "medium", "dark"):
        write(f"assets/{ID}/textures/gui/hz/monitor_light_{light}.png", read(f"assets/{ID}/textures/block/crafting/monitor_light_{light}.png"))
    write(f"assets/{ID}/textures/gui/hz/molecular_assembler_lights.png", read(f"assets/{ID}/textures/block/molecular_assembler_lights.png"))
    write(f"assets/{ID}/textures/gui/hz/crafting_fx.png", read(f"assets/{ID}/textures/particle/energy.png"))
    charger_models(write, read)
    compass_pointers(write, read)
    facades(write, read, has)
    # The sky stone chests: the base as the block, the lid (and its knob) as a block of its own the native mod
    # draws opening, as AE2's SkyChestTESR does.
    chest = json.loads(read(f"assets/{ID}/models/item/sky_stone_chest.json"))
    for name, tex in (("sky_stone_chest", "skychest"), ("smooth_sky_stone_chest", "skyblockchest")):
        textures = {"0": f"{ID}:block/{tex}", "particle": f"{ID}:block/{tex}"}
        base = [e for e in chest["elements"] if e.get("name") == "Base"]
        lid = [e for e in chest["elements"] if e.get("name") != "Base"]
        write(f"assets/{ID}/models/block/hz/{name}_base.json", {"textures": textures, "elements": base})
        write(f"assets/{ID}/models/block/hz/{name}_lid.json", {"textures": textures, "elements": lid})
        write(f"data/{ID}/horizon/block/hz_{name}_lid.json", {"template": "minecraft:glass", "properties": {"hardness": 0.0}})
        write(f"assets/{ID}/blockstates/hz_{name}_lid.json", {"variants": {"": {"model": f"{ID}:block/hz/{name}_lid"}}})
    # AE2's level emitter: its part model against the block's back face, off and on (it glows while emitting).
    looks = {}
    for state in ("off", "on"):
        elements, textures = part_elements(read, f"{ID}:part/level_emitter_base_{state}")
        textures = {k: (PART_TEXTURES[1] + v[len(PART_TEXTURES[0]):] if v.startswith(PART_TEXTURES[0]) else v) for k, v in textures.items()}
        depth = max((e["to"][2] for e in elements), default=2)
        moved = json.loads(json.dumps(elements))
        for e in moved:
            e["from"][2] += 16 - depth
            e["to"][2] += 16 - depth
            if "rotation" in e and "origin" in e["rotation"]:
                e["rotation"]["origin"][2] += 16 - depth
        if not any("south" in e["faces"] for e in moved):
            # AE2's lit emitter has no back (its cable hides it): the unlit one's cap, here where it can be seen.
            textures.setdefault("sides", f"{ID}:block/part/monitor_sides")
            moved.append({"from": [7, 7, 15], "to": [9, 9, 16], "faces": {"south": {"texture": "#sides"}}})
        _inset(moved)
        emitter_elements = moved
        write(f"assets/{ID}/models/block/hz_part/level_emitter_{state}.json", {"textures": textures, "elements": moved})
        looks[state] = {"model": f"{ID}:block/hz_part/level_emitter_{state}"}
    write(f"data/{ID}/horizon/block/level_emitter.json",
          {"template": "minecraft:barrel", "properties": {"hardness": 0.3, "requires_tool": False, "sounds": "minecraft:iron_block"},
           **POWERED_STATE, "shapes": part_shapes(emitter_elements)})
    write(f"assets/{ID}/blockstates/level_emitter.json", facing_variants(looks["off"], powered=(looks["off"], looks["on"])))
    write(f"data/{ID}/loot_table/blocks/level_emitter.json", self_drop("level_emitter"))
    # A block now, as the part blocks are: its item is the block's, with AE2's item model.
    if "level_emitter" not in blocks:
        blocks.append("level_emitter")
    PART_BLOCKS.setdefault("level_emitter", ("level_emitter_base_off", None))
    write(f"assets/{ID}/items/level_emitter.json", {"model": {"type": "minecraft:model", "model": f"{ID}:item/level_emitter"}})
    # AE2's energy level emitter: the level emitter's part (AE2 draws it the same), a block of its own.
    write(f"data/{ID}/horizon/block/energy_level_emitter.json",
          {"template": "minecraft:barrel", "properties": {"hardness": 0.3, "requires_tool": False, "sounds": "minecraft:iron_block"},
           **POWERED_STATE, "shapes": part_shapes(emitter_elements)})
    write(f"assets/{ID}/blockstates/energy_level_emitter.json", facing_variants(looks["off"], powered=(looks["off"], looks["on"])))
    write(f"data/{ID}/loot_table/blocks/energy_level_emitter.json", self_drop("energy_level_emitter"))
    if "energy_level_emitter" not in blocks:
        blocks.append("energy_level_emitter")
    PART_BLOCKS.setdefault("energy_level_emitter", ("level_emitter_base_off", None))
    write(f"assets/{ID}/items/energy_level_emitter.json", {"model": {"type": "minecraft:model", "model": f"{ID}:item/energy_level_emitter"}})
    # AE2's planes (drawn by a model loader in Java): a plate on the face it works on, its sides AE2's plane sides,
    # the animated texture while it works.
    # AE2's PlaneBakedModel: a plate a pixel thick, a pixel short of the block's edge, except toward a plane of the
    # same kind on the same face beside it (the plates join into one). The plate's middle, then a strip per side
    # and a corner per pair, shown by the `link_*` states the native mod sets; the front is the plane's texture,
    # the back transition_plane_back, the edges plane_sides. Model axes (front north): +x east, +y up.
    links = {"link_xn": (0, 1, 1, 15), "link_xp": (15, 16, 1, 15), "link_yn": (1, 15, 0, 1), "link_yp": (1, 15, 15, 16)}
    corners = {("link_xn", "link_yn"): (0, 1, 0, 1), ("link_xp", "link_yn"): (15, 16, 0, 1),
               ("link_xn", "link_yp"): (0, 1, 15, 16), ("link_xp", "link_yp"): (15, 16, 15, 16)}

    def plate(x0, x1, y0, y1, edges):
        side = {"texture": "#sides"}
        faces = {"north": {"texture": "#plane"}, "south": {"texture": "#back"}}
        for d in edges:
            faces[d] = dict(side)
        return {"from": [x0, y0, 0], "to": [x1, y1, 1], "faces": faces}

    mount = {"from": [5, 5, 1], "to": [11, 11, 2],
             "faces": {d: {"texture": "#sides"} for d in ("north", "south", "east", "west", "up", "down")}}
    for plane in ("annihilation_plane", "formation_plane"):
        looks = {}
        for state, tex in (("off", plane), ("on", f"{plane}_on")):
            textures = {"plane": f"{ID}:block/part/{tex}", "sides": f"{ID}:block/part/plane_sides",
                        "back": f"{ID}:block/part/transition_plane_back", "particle": f"{ID}:block/part/plane_sides"}
            pieces = {"middle": [plate(1, 15, 1, 15, ("east", "west", "up", "down")), dict(mount)]}
            for name, (x0, x1, y0, y1) in links.items():
                edge = {"link_xn": ("west", "up", "down"), "link_xp": ("east", "up", "down"),
                        "link_yn": ("down", "east", "west"), "link_yp": ("up", "east", "west")}[name]
                pieces[name] = [plate(x0, x1, y0, y1, edge)]
            for (a, b), (x0, x1, y0, y1) in corners.items():
                pieces[f"{a}_{b}"] = [plate(x0, x1, y0, y1, ("east", "west", "up", "down"))]
            for piece, elements in pieces.items():
                write(f"assets/{ID}/models/block/hz_part/{plane}_{state}_{piece}.json",
                      {"textures": textures, "elements": _inset(json.loads(json.dumps(elements)))})
            looks[state] = pieces
        full = [plate(0, 16, 0, 16, ()), dict(mount)]
        write(f"data/{ID}/horizon/block/{plane}.json",
              {"template": "minecraft:barrel", "properties": {"hardness": 0.3, "requires_tool": False, "sounds": "minecraft:iron_block"},
               "state_properties": {**{k: {"type": "boolean"} for k in links}, "powered": {"type": "boolean"}},
               "default_state": {**{k: "false" for k in links}, "powered": "false"},
               "shapes": part_shapes(_inset(full), stub=True)})
        parts = []
        for facing, rot in FACING_ROTATIONS.items():
            for on, state in (("false", "off"), ("true", "on")):
                model = lambda piece: {"model": f"{ID}:block/hz_part/{plane}_{state}_{piece}", **rot}
                parts.append({"when": {"facing": facing, "powered": on}, "apply": model("middle")})
                for name in links:
                    parts.append({"when": {"facing": facing, "powered": on, name: "true"}, "apply": model(name)})
                for a, b in corners:
                    parts.append({"when": {"facing": facing, "powered": on, a: "true", b: "true"}, "apply": model(f"{a}_{b}")})
        write(f"assets/{ID}/blockstates/{plane}.json", {"multipart": parts})
        write(f"data/{ID}/loot_table/blocks/{plane}.json", self_drop(plane))
        write(f"assets/{ID}/items/{plane}.json", {"model": {"type": "minecraft:model", "model": f"{ID}:item/{plane}"}})
        if plane not in blocks:
            blocks.append(plane)
        PART_BLOCKS.setdefault(plane, ("plane", None))
    # AE2's access point item lies on its side (antenna along +x): stood up and shown larger, as AE2's item looks.
    upright = lambda r, scale, t=(0, 0, 0): {"rotation": [r[0], r[1], 90], "translation": list(t), "scale": [scale] * 3}
    write(f"assets/{ID}/models/block/hz/wireless_access_point_item.json", {
        "parent": f"{ID}:item/wireless_access_point",
        "display": {
            "gui": upright((30, 225), 0.9, (0, -1, 0)),
            "ground": upright((0, 0), 0.4, (0, 2, 0)),
            "fixed": upright((0, 0), 0.8),
            "thirdperson_righthand": upright((75, 45), 0.5, (0, 2.5, 0)),
            "firstperson_righthand": upright((0, 45), 0.6),
            "firstperson_lefthand": upright((0, 225), 0.6),
        },
    })
    write(f"assets/{ID}/items/wireless_access_point.json",
          {"model": {"type": "minecraft:model", "model": f"{ID}:block/hz/wireless_access_point_item"}})
    # The crank's handle as a block of its own, never placed (the native mod draws it turning).
    write(f"data/{ID}/horizon/block/hz_crank_handle.json", {"template": "minecraft:glass", "properties": {"hardness": 0.0}})
    write(f"assets/{ID}/blockstates/hz_crank_handle.json", {"variants": {"": {"model": f"{ID}:block/crank_handle"}}})
    crafting_cube_models(write, read)
    # Fabric's common tags AE2's recipes name and the jar doesn't ship (Fabric API has them): vanilla's members,
    # so the dye recipes (coloured cables, paint balls) resolve.
    # AE2's guide: its Markdown pages (the native mod draws them) and which page is about which item.
    guide_items, guide_titles = {}, {}
    guide_parents, guide_positions, guide_icons, guide_categories = {}, {}, {}, {}
    for n in names:
        if not (n.startswith(f"assets/{ID}/ae2guide/") and n.endswith(".md")):
            continue
        page = n[len(f"assets/{ID}/ae2guide/"):]
        text = read(n).decode("utf-8", "replace")
        write(f"data/{ID}/ae2guide/{page}", text.encode("utf-8"))
        if text.startswith("---") and "\n---" in text[3:]:
            front = text[3:text.index("\n---", 3)]
            title = re.search(r"^\s*title:\s*(.+)$", front, re.M)
            if title:
                guide_titles[page] = title.group(1).strip()
            # Navigation (SubPages, CategoryIndex): the parent page, the order, the icon and the categories.
            for key, store in (("parent", guide_parents), ("position", guide_positions), ("icon", guide_icons)):
                m = re.search(rf"^\s*{key}:\s*(.+)$", front, re.M)
                if m:
                    store[page] = m.group(1).strip()
            cats = re.search(r"^categories:[ \t]*\n((?:[ \t]*-[ \t]*.+\n?)+)", front + "\n", re.M)
            if cats:
                guide_categories[page] = [c.strip()[1:].strip() for c in cats.group(1).splitlines() if c.strip().startswith("-")]
            in_items = False
            for line in front.splitlines():
                if line.startswith("item_ids:"):
                    in_items = True
                    continue
                if in_items:
                    m = re.match(r"^\s*-\s*(\S+)", line)
                    if not m:
                        in_items = False
                        continue
                    item = m.group(1) if ":" in m.group(1) else f"{ID}:{m.group(1)}"
                    guide_items.setdefault(item, page)
    # Items AE2's pages describe without listing them in their front matter: W opens that page too.
    for size in ("1k", "4k", "16k", "64k", "256k"):
        for kind in ("item", "fluid"):
            guide_items.setdefault(f"{ID}:portable_{kind}_cell_{size}", "items-blocks-machines/storage_cells.md")
    write(f"data/{ID}/ae2guide/index.json", {"items": guide_items, "titles": guide_titles, "parents": guide_parents,
                                             "positions": guide_positions, "icons": guide_icons, "categories": guide_categories})
    # The guide's pictures, as GUI sprites the native guide draws (`ae2:gui/guide/<path>`).
    for n in names:
        if n.startswith(f"assets/{ID}/ae2guide/") and n.endswith(".png"):
            # At most 480 by 256: Horizon's GUI atlas leaves out what won't pack (the guide draws them smaller).
            im = Image.open(io.BytesIO(read(n))).convert("RGBA")
            if im.width > 480 or im.height > 256:
                im.thumbnail((480, 256), Image.LANCZOS)
            buf = io.BytesIO()
            im.save(buf, "PNG")
            write(f"assets/{ID}/textures/gui/guide/{n[len(f'assets/{ID}/ae2guide/'):]}", buf.getvalue())
    # The guide's scenes (assets/assemblies/*.snbt), as the port's world has them: each block with its state's
    # properties; a cable bus as its cable (arms toward its connections and parts, its channel lights) and each
    # part as the part block on that side, facing out, its back on the cable.
    dirs = {"north": (0, 0, -1), "south": (0, 0, 1), "east": (1, 0, 0), "west": (-1, 0, 0), "up": (0, 1, 0), "down": (0, -1, 0)}
    for n in names:
        if not (n.startswith(f"assets/{ID}/ae2guide/") and n.endswith(".snbt")):
            continue
        blocks_in = []
        for line in read(n).decode("utf-8", "replace").splitlines():
            m = re.search(r'pos: \[(-?\d+), (-?\d+), (-?\d+)\], state: "([^"{]+)(\{[^"}]*\})?', line)
            if not m:
                continue
            bx, by, bz, state = int(m.group(1)), int(m.group(2)), int(m.group(3)), m.group(4)
            props = dict(kv.split(":", 1) for kv in (m.group(5) or "{}")[1:-1].split(",") if ":" in kv)
            if state == "minecraft:air":
                continue
            if state.endswith(":cable_bus"):
                parts = {}
                for pm in re.finditer(r'\b(cable|north|south|east|west|up|down): \{', line):
                    im = re.compile(r'\bid: "([^"]+)"').search(line, pm.end())
                    if im:
                        parts[pm.group(1)] = im.group(1)
                cable = parts.pop("cable", None)
                if cable:
                    cm = re.search(r'cable: \{.*?visual: \{([^}]*)\}', line)
                    visual = cm.group(1) if cm else ""
                    conn = re.search(r'connections: \[([^\]]*)\]', visual)
                    joined = set(re.findall(r'"(\w+)"', conn.group(1))) if conn else set()
                    joined |= set(parts)
                    arms = {d: ("true" if d in joined else "false") for d in dirs}
                    channels = [int(c) for c in re.findall(r'channels\w+: (\d+)', visual)]
                    if "smart" in cable:
                        most = max(channels, default=0)
                        arms["x_lights"] = str(min(8, -(-most // 4) if "dense" in cable else most))
                    blocks_in.append([bx, by, bz, cable, arms])
                back = {"north": "south", "south": "north", "east": "west", "west": "east", "up": "down", "down": "up"}
                for side, part in parts.items():
                    dx, dy, dz = dirs[side]
                    blocks_in.append([bx + dx, by + dy, bz + dz, part, {"facing": side, "powered": "true", "with_channel": "true"}])
                    # A bus, plane or tunnel holds the neighbour's own cable as a stub toward it (the native mod
                    # draws it in the world: part_cable_submit).
                    if cable and re.search(r"_(bus|p2p_tunnel|plane)$", part):
                        stub = {d: ("true" if d == back[side] else "false") for d in dirs}
                        if "smart" in cable:
                            stub["x_lights"] = arms.get("x_lights", "0")
                        blocks_in.append([bx + dx, by + dy, bz + dz, cable, stub])
                continue
            blocks_in.append([bx, by, bz, state, props])
        page = n[len(f"assets/{ID}/ae2guide/"):-len(".snbt")] + ".json"
        write(f"data/{ID}/ae2guide/{page}", {"blocks": blocks_in})
    glass = lambda n: read(f"assets/{ID}/textures/block/glass/{n}.png")
    write(f"assets/{ID}/textures/block/hz/quartz_glass.png", compose(glass("quartz_glass_a"), [(glass("quartz_glass_frame1111"), 0xFFFFFF)]))

    # Items, and every item's client definition.
    items = sorted(
        k[len(f"item.{ID}."):]
        for k in lang
        if k.startswith(f"item.{ID}.") and k.count(".") == 2 and "debug" not in k
    )
    for name in items:
        if name in blocks:
            continue
        write(f"data/{ID}/horizon/item/{name}.json", item_decl(name))
        report["items"] += 1
    multiblock_models(write)
    for fluid, tint in (("water", -12618012), ("lava", None)):
        write(f"assets/{ID}/models/item/hz_fluid_{fluid}.json",
              {"parent": "minecraft:item/generated", "textures": {"layer0": f"minecraft:block/{fluid}_still"}})
        model = {"type": "minecraft:model", "model": f"{ID}:item/hz_fluid_{fluid}"}
        if tint is not None:
            model["tints"] = [{"type": "minecraft:constant", "value": tint}]
        write(f"assets/{ID}/items/hz_fluid_{fluid}.json", {"model": model})
    add_registries(write, blocks, items)
    # Parts and cables are items in AE2, named item.ae2.*: as blocks their screens and tooltips read block.ae2.*.
    for lang_file in [n for n in names if re.fullmatch(rf"assets/{ID}/lang/[a-z_]+\.json", n)]:
        try:
            table = json.loads(read(lang_file))
        except ValueError:
            continue
        added = {f"block.{ID}.{b}": table[f"item.{ID}.{b}"] for b in blocks
                 if f"item.{ID}.{b}" in table and f"block.{ID}.{b}" not in table}
        # The facade twins are AE2's cable bus block.
        if f"block.{ID}.cable_bus" in table:
            added.update({f"block.{ID}.{t}": table[f"block.{ID}.cable_bus"] for t in TWINS})
        if added:
            write(lang_file, {**table, **added})
    for name in [n for n in items if n not in PART_BLOCKS] + [b for b in blocks if b not in NO_ITEM and b not in PART_BLOCKS]:
        if name == "wireless_access_point":
            model = f"{ID}:block/hz/wireless_access_point_item"
        elif f"assets/{ID}/models/item/{name}.json" in has:
            model = f"{ID}:item/{name}"
        elif f"assets/{ID}/models/block/{name}.json" in has:
            model = f"{ID}:block/{name}"
        else:
            continue
        write(f"assets/{ID}/items/{name}.json", {"model": {"type": "minecraft:model", "model": model}})

    # Data: recipes, loot, tags. Worldgen (meteorites), the spatial dimension and the
    # advancements need AE2's code.
    for n in names:
        if not n.startswith("data/") or not n.endswith(".json"):
            continue
        ns, rest = n.split("/", 2)[1:]
        for old, new in RENAMED_DIRS:
            if rest.startswith(old):
                rest = new + rest[len(old):]
        if ns in ("minecraft", "c"):
            if not rest.startswith("tags/") or rest.startswith("tags/point_of_interest_type/"):
                continue
        elif ns != ID or not rest.startswith(("recipe/", "loot_table/", "tags/")):
            continue
        v = json.loads(read(n))
        dropped = lambda ref: ref.startswith(f"{ID}:debug_") or ref[len(ID) + 1:] in NO_ITEM
        if rest.startswith("tags/"):
            # Tags name nothing that isn't declared: a tag with an unknown entry fails to load.
            v["values"] = [x for x in v.get("values", []) if not (isinstance(x, str) and dropped(x))]
        elif rest.startswith("loot_table/blocks/") and dropped(f"{ID}:{rest[len('loot_table/blocks/'):-5]}"):
            continue
        if rest.startswith("recipe/"):
            v = upgrade_recipe(v)
            if v is None:
                report["recipes_left_out"].append(rest)
                continue
            report["recipes"] += 1
        elif rest.startswith("loot_table/"):
            v = upgrade_loot(v)
        write(f"data/{ns}/{rest}", v)

    write(
        "horizon-mod.toml",
        (
            "# AE2's blocks and items, carried over from appliedenergistics2-fabric-15.4.12.jar by\n"
            "# ae2-horizon/tools/make_content_mod.py. Licences: code LGPL-3.0, textures and models CC BY-NC-SA 3.0 (TeamAppliedEnergistics, Ridanisaurus).\n"
            f'id = "{ID}"\n'
            'name = "Applied Energistics 2 (blocks and items)"\n'
            'version = "15.4.12"\n'
            'description = "AE2\'s blocks, items, recipes and textures. Blocks behave like the vanilla block they copy."\n'
            'authors = ["TeamAppliedEnergistics"]\n'
            'api = "0.2"\n'
            'side = "both"\n'
            'capabilities = ["registries"]\n'
        ).encode(),
    )
    report["recipes_left_out"].sort()
    (out.parent / f"{out.name}-report.json").write_text(json.dumps(report, indent=2))
    merge_convention_tags(jar, out)
    # AE2's cable bus and paint splats are drawn by its code (never a model): empty models, so nothing is missing.
    for name in ("cable_bus", "paint"):
        p = Path(out) / f"assets/{ID}/models/block/{name}.json"
        if not p.exists():
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(json.dumps({"textures": {"particle": f"{ID}:block/sky_stone_block"}, "elements": []}))
    # Paint balls: AE2 tints one white ball per colour in code; here each colour gets its own texture (medium
    # colour for paint, bright for lumen paint, whose shimmer keeps its animation).
    tex = Path(out) / f"assets/{ID}/textures/item"
    for kind, base, shade in (("paint_ball", "paint_ball", 0), ("lumen_paint_ball", "paint_ball_shimmer", 1)):
        src = tex / f"{base}.png"
        if not src.exists():
            continue
        # Pillow: the ball is a palette PNG with transparency, which png_tint's reader doesn't take.
        ball = Image.open(src).convert("RGBA")
        for color in COLORS:
            name = f"{color}_{kind}"
            (tex / "hz").mkdir(parents=True, exist_ok=True)
            tint = CABLE_TINTS[color][shade]
            r, g, b = tint >> 16 & 255, tint >> 8 & 255, tint & 255
            px = [(pr * r // 255, pg * g // 255, pb * b // 255, pa) for pr, pg, pb, pa in ball.getdata()]
            out_img = Image.new("RGBA", ball.size)
            out_img.putdata(px)
            out_img.save(tex / f"hz/{name}.png")
            meta = tex / f"{base}.png.mcmeta"
            if meta.exists():
                (tex / f"hz/{name}.png.mcmeta").write_bytes(meta.read_bytes())
            model = Path(out) / f"assets/{ID}/models/item/{name}.json"
            if model.exists():
                m = json.loads(model.read_text())
                m["textures"] = {"layer0": f"{ID}:item/hz/{name}"}
                model.write_text(json.dumps(m, indent=2))
    see_through = fix_occlusion(out)
    print(f"no longer hiding faces: {len(see_through)} blocks")
    print(
        f"{len(report['blocks'])} blocks, {report['items']} items, {report['recipes']} recipes "
        f"({len(report['recipes_left_out'])} left out), {report['files']} files -> {out}"
    )


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
