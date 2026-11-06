# AE2 on Horizon: working plan

Target: Horizon Nightly `c9d474dd9116e84c243f8992d75c11314df56f06` (the player's game, native hooks API 80) and
horizon-mixins `0d6b103c70633d8fd9443d8d0acd3aadaacedb06`. Source and docs for that commit are in the launcher's
build folder: `%APPDATA%/horizon-launcher/native/source/c9d474dd…/` (`docs/native-mods.md`, `docs/mods.md`,
`crates/horizon-client/src/game/native_hooks.rs`, `native-hooks.json`). Read those, not older copies.

Goal: a full ME system in single player — controller, cables, drive with cells, terminal, crafting terminal,
patterns, pattern provider, crafting CPU, import/export buses — with AE2's own screens.

## Layers

1. **Pure logic** (done, `cargo test`): `core.rs` keys/counters/cells, `crafting.rs` calculator, `execution.rs`
   crafting CPU, `grid.rs` grids/channels/energy/network storage. Later moved into a `[[crate]]` so editing glue
   doesn't recompile it.
2. **Content mod** (`tools/make_content_mod.py` → `ae2-content/`): blocks, items, models, textures, recipes, plus:
   - cables: template `minecraft:chorus_plant`, `connects_to: "#ae2:cable_connects"`, `thickness`;
   - machines that hold items or open a screen: a template with a block entity and a menu (`minecraft:barrel`:
     facing in six directions), so `native_block_entity` gives them a mod block entity;
   - `data/ae2/horizon/menu/*.json` menu types; `textures/guis/` copied to `textures/gui/` for `native_sprite`;
   - a creative tab with every AE2 item.
3. **Native glue** (hooks by name from `native-hooks.json`):
   - `native_block_entity`, `_ticks`, `_container`, `_tick`: drive (10 cells), ME chest (1 cell), terminals,
     interface, pattern provider, buses, crafting units. Cell contents ride on the cell item (custom data).
   - Server tick: rebuild grids from block-entity positions by walking cables (`native_block`), run `grid.rs`,
     power, crafting CPUs (`execution.rs`), buses.
   - Screens: `native_screen` + `native_screen_draw` (AE2 GUI textures), `native_screen_click` → button ids →
     `native_menu_button_v2` (has the cursor stack) on the server; scroll and typing hooks for the terminal search.
   - Mod messages (`native_net_*`, channel `ae2:*`): the server streams the network's items to open terminals.

## Milestones (each ends with a native build and an in-game test)

- M1: cables connect; controller/drive/terminal form a network; cells go in the drive's screen; the terminal
  shows network items and takes/gives items.
- M2: crafting terminal; pattern encoding; pattern provider; crafting CPU and craft-amount/confirm screens.
- M3: import/export/storage buses; vibration chamber and energy cells.
- M4: polish: sort/search buttons, channels display, tooltips.

## Status (2026-11-02, after the user's third test)

Done: content mod (cables as chorus pipes per colour, barrel devices, menus, tab, GUI textures, part models flush
to the cable), M1 native (`mixins/ae2/me.rs` + `me_*.rs` helpers): block entities, grids, power (creative cell,
vibration chamber, resonance generator), drive/chest cells on the cell item, terminal screen with streaming and
clicks (taken items go to the inventory), charger.

Open from the user's reports:
- terminal menu broke when its only slot was hidden → slot back, placed off-screen (shift-click inserts again);
- terminal rows should grow with the window (`game.gui_size()`), AE2 texture drawn in strips;
- part items need display transforms (front shown); upright machines (charger…) ignore up/down facing;
- powered looks: `powered` state property on controller/terminals, set from the server's grid pass;
- charger item and drive cells not drawn (update tag + `native_block_entity_submit`): debug with `log::info!`
  and the agent (`horizon-ctl`, world "AE2 Agent Test");
- smart/dense cable channel lights;
- then M2 (crafting terminal, patterns, pattern provider, CPU) and M3 (import/export/storage buses).

Testing rule: after the user's report, verify with horizon-ctl in "AE2 Agent Test" first; then launch the user's
own world ("New World (1)") for them.

## Status (later, 2026-11-02)

- M1 verified by the agent (`tools/agent_m1.sh <build>`): network powers up, drive screen + cell drawn on its
  face (drive/chest/charger copy the beacon: an occluding block culls its block entity's drawing), terminal
  stores (shift-click, click with stack) and takes (click → cursor, right-click half, shift → inventory),
  charger charges a crystal. Fixed on the way: units of `native_draw_cube` are texels; menu buttons have no
  acting player (the viewer is found by terminal position); `click-at` coordinates come from `ui`'s gui_size.
- M2 written and compiled (build native-c9d474d-6985a5086704), not yet agent-tested: crafting terminal (grid +
  result, refills from network), pattern encoding terminal (Encode / Crafting↔Processing buttons; processing =
  smelting of the first input), pattern provider (9 slots; crafting patterns need an adjacent molecular
  assembler, processing patterns are pushed into the adjacent machine and its output collected), crafting CPU
  from crafting storage blocks + accelerators, craftables in the terminal (click 1 / right 16 / shift 64).
- Disk: C: was full. Launcher versions live in `Z:\AE2 Converter\horizon-builds\versions` (junctions); run
  `powershell -ExecutionPolicy Bypass -File tools\offload_versions.ps1` after each build. `native\target` must
  stay a real folder on C: (rustc fails to make temp dirs through a junction).
- The user asked not to close their running Minecraft: ask before quitting a game to test a new build.

## User's report on the M2 build (2026-11-02, 17:20) — to do

1. Drive drawn/positioned wrong (screenshot in %APPDATA%\horizon\screenshots).
2. Charger: AE2's electric particles / animation while charging.
3. Terminal shows and gives items while unlit (offline): hide the grid and refuse clicks when not powered.
4. Terminal lags a moment before items update: stream at once after any storage change (and faster than 10 ticks).
5. Terminal scroll bar still mispositioned.
6. Controller lit without power: `powered` must follow real power, not just the grid state.
7. Controller has no UI (AE2's network status screen: channels, power, devices).
8. Growth accelerator: directional, powered (changes look), particles, speeds up crystal growth.
9. Budding quartz: grows quartz buds → clusters on its faces (random ticks), as AE2/amethyst does.
10. Inscriber: its menu (4 slots + progress), recipes (`ae2:inscriber` recipe type, via native_recipes_of_type),
    and its press animation.
11. Molecular assembler: animation while crafting (and its own UI/pattern slot, AE2-style).
12. Upgrades (cards): acceleration, capacity, fuzzy, inverter, redstone… in cells, buses, interfaces.
Then M3: import/export/storage buses.

## User's report on M2 (17:45) — to do
1. Crafting terminal doesn't grow with the window (rows fixed at 3).
2. Encoder: show the recipe's product (preview) on the right once inputs are in.
3. Crafting terminal amounts overlapped "Craft" (now "+").
4. Drive: inserted cells still positioned wrong.
5. Molecular assembler: UI (pattern slot + progress) and animation.
6. Middle-click a craftable item → "how many to craft" screen (amount input + confirm).
7. Crafting multiblock: should form (AE2 formed textures, cuboid rules).
8. Crafting CPU status UI (what's crafting, progress).
Verified by the agent: encoding, provider, CPU, assembler → "Crafting finished", 4 planks in the network.

## Status (night of 2026-11-02, goal: 1:1 port, billions of items without lag)

Build folder: `native-build/` (junctions to this bundle and the user's untouched `jei-src`, JEI 0.1.0); toolchain
and cargo on `Z:\AE2Converter-build` (no spaces: MinGW's linker splits paths), `native\target` stays on C:.
Agent tests: `tools/agent_m4.sh`..`agent_m7.sh` (use the game's `/tp`, `MSYS_NO_PATHCONV=1`; `/ae2debug` via `chat`).

Done and agent-tested:
- Storage at scale: cells decoded once and cached by an id on the cell (`ae2:cell_id/ver/status`), each grid's
  storage kept mounted, an item→cells index and running totals in `NetworkStorage` (grid.rs), terminals sent only
  what changed (indexed diff, chunked, `#reset`), client list cached and sorted once. `/ae2debug synth 1000 20000`:
  1.8 billion items, 18 000 types: full list 1.2–3 ms, 1000 take+put 0.75–1.4 ms (was 16 ms and 75 ms).
  In world: 1 812 types / 45 M items, terminal list 2.5 ms.
- Terminals: AE2 style header (title, search box), left toolbar (sort, view, crafting status), small amounts,
  "Craft", tall rows; crafting status overlay; crafting terminal shift-click stores into the network.
- Pattern encoding terminal on AE2's pattern.png/pattern_modes.png, ghost slots, tabs, scales with the window.
- JEI: AE2 tabs and "+" transfer as a layer over the original JEI (jei.rs); JEI untouched.
- Pattern access terminal (the "interface terminal"): providers grouped by their machine, click to add/take.
- Cables use AE2's own part models, smart/dense channel lights (x_lights), no z-fighting; cable shapes repaired.
- Sky stone chests (inventory, AE2 screen, lid animation and sounds), wooden crank (smooth, charges), assembler UI,
  CPU multiblock formed looks and per-cluster CPUs, inscriber animation, charger ports.

Also done: upgrade cards (inscriber 3, assembler 5, buses 4/5 slots, AE2's panel; acceleration cards speed them),
the guide (W over any item, slot or JEI; 120 pages, links, Back), item names in terminal tooltips.

Also done (agent-tested, build 7af91b25c86e): interface (config ghosts, stocks from the network), IO port (empty/fill
cells, speed cards), level emitter (config item, typed level, real redstone via native_block_signal), one card per
upgrade slot, coloured cable recipes (Fabric c:*_dyes / glass_blocks tags added), JEI mob drops tab (rolls each
spawn-egg mob's loot table 400x with the game's loot code: real chances), guide scenes (assemblies/*.snbt ->
isometric, blocks clickable), JEI panel hidden while the guide is open.

Wireless (build 02beaec4e5bd, agent-tested 2026-11-03): access point links a terminal (ae2:link in custom_data),
boosters raise range, use opens a mod screen with the terminal grid + inventory; take and put back work.

Still to do: fluids (cells, terminal, buckets), energy acceptor (Horizon has no other mods' energy API), spatial IO, quartz
tools (wrench, knife, cutting), P2P tunnels, facades, annihilation/formation planes, charger has no screen in AE2.

## Status (2026-11-03, build 2ac8629e5419)

Mods (native-build/ junctions; each its own [mod] in horizon-mixin.toml):
- `ae2` (mixins/ae2): Applied Energistics 2 core. Content mod: ae2-content (make_content_mod.py).
- `ae2_jei` (mixins/ae2-jei): AE2 Native JEI: jei.rs (categories incl. Entropy + Attunement, information tab from
  the guide, mob drops, "+" transfer) over the original JEI (jei-src, untouched).
- `jade` (mixins/jade): Jade port: top-centre overlay (icon, name, harvest tick, growth, power, mob health/armour,
  mod name), provider registry (register_client / register_server), server lines over jade:request/jade:data.
- `jade_ae2` (mixins/jade-ae2): AE2's Jade plugin: port_ae2_me::describe (device state, channels, P2P link and
  frequency, charger %, crafting job, stored AE, cells).
- Fabric API: its convention tags (fabric-convention-tags-v2 nested jar) merged into ae2-content's data/c/tags,
  with c:<color>_dyes aliases for AE2's older names. AE2's 74 left-out recipes are matter-cannon ammo gated on
  other mods' tags (fabric:tags_populated): correctly absent.

Pipeline: tools/agent_m11.sh (22 checks + Jade shots). 20/22 pass. Open: wireless put-back passes by hand but
not in the pipeline (timing); cooling water by a sneaking click through the water still heats/does nothing
(the click reaches use_item_on on the block below; needs a check).
Done since last status: fluid cells (were never mounted), fluids in terminals by bucket, P2P (ME/item/redstone/
fluid/light/FE kinds, attunement, memory card links), annihilation + formation planes, spatial IO, wireless
access point + terminal (tooltips), quartz tools (wrench, network tool, memory card, entropy manipulator,
colour applicator, cutting knife), card limits (one per slot), empty-cell inserts in terminals.

## Status (2026-11-03, build 5f33aba38f34): pipeline 26/26 headless

Fixed since: non-cube blocks no longer hide neighbours' faces (fix_occlusion pass: 20 barrel parts -> beacon
template, 4 plain -> glass); parts placed against a block face it (buses, tunnels, planes) or face the player
(terminals, emitters, access point); P2P models sit at the front; info screens for P2P tunnels, planes, energy
cells (AE2's lines via describe), formation plane filter row; paint balls coloured per colour; empty models for
cable_bus/paint; W with nothing hovered opens the looked-at block's guide page; Jade hidden under screens;
water cooling through water works (tool_click shared by right_click and use_item_on).
Texture audit: every model texture resolves except AE2's raw cable templates' placeholders (unused).

UIs still missing vs AE2's screens/: craft amount / confirm / error (crafting requests use 1/16/64 clicks),
priority, cell workbench, condenser, energy level emitter, quantum network bridge, spatial anchor, portable
item/fluid cells, network tool inventory, quartz knife naming, terminal settings, pattern encoder smithing and
stonecutting modes, set stock / processing amounts.

## Status (2026-11-03, later): the missing screens

Added (me.rs, "More of AE2's screens"): a dialog layer over any AE2 screen (`ae2:ui` channel) with AE2's craft
amount (+1..+1000, Next), crafting plan confirm (stored / to craft / missing, bytes, CPU, Back/Start) and error
(Replan/Retry/Cancel) screens; priority (drive, ME chest, storage bus, formation plane; used as mount priority);
set stock amount (shift-click an interface config cell) and processing amounts (shift-click an encoder cell);
terminal settings (gear under the toolbar: auto-focus search, remember search, ask how many to craft; saved to
%APPDATA%/horizon/ae2-client.txt); pattern encoder smithing and stonecutting modes (their patterns run in
molecular assemblers); cell workbench (63-cell partition onto the cell, partition/clear buttons, used by
drives); condenser (trash / matter balls / singularities, storage component capacity); energy level emitter
(new block, the level emitter's model); quantum network bridge (3x3 ring check in any plane, entangled
singularity frequencies, links grids in `rebuild`; a singularity + ender dust in an explosion gives the nearest
player a pair, since items can't survive the blast here); spatial anchor screen (AE2's statistics; Horizon has
no chunk forcing, it says so); item screens over the world: portable item cells (terminal of 3 rows; fluid
portables list only), network tool toolbox (9 card stacks on the tool) and network status (power, devices grid,
rates), cutting knife (name box, iron in, named Name Press out, knife wears).
Still not portable to Horizon: chunk forcing (spatial anchor), other mods' energy (energy acceptor), card
upgrades on cells in the workbench.

## Audit (2026-11-03): what is still not 1:1 (from the jar's menus, commands, textures and items)

Connections / multiblocks:
- Parts (buses, terminals, planes, tunnels, emitters) join the network on all six sides; AE2 joins a part only
  through its back (its cable). A storage bus facing an interface merges the two networks here.
- Controller multiblock looks: AE2's block / column_x|y|z / inside_a|b and online/offline/conflicted models
  (the conflict rule itself is ported); quantum bridge formed models (ring lights, corners); spatial pylon
  column looks (base/dim/red, end/spanned).
Screens: ME chest opens its cell's terminal when powered (here only the cell slot); wireless crafting terminal;
  pattern provider, storage bus, vibration chamber and crafting CPU screens draw plain panels instead of AE2's
  textures; AE2's states.png icons, checkbox, text_field, cpu_selector widgets not used.
Commands: `/ae2 channelmode|chunklogger|reloadconfig|setuptestworld|spatial|testmeteorites|tickmonitoring`.
Items with no behaviour yet: storage monitor, conversion monitor, toggle bus, inverted toggle bus, quartz fiber,
  facades, cable anchor, illuminated panels (monitor, semi-dark, dark), matter cannon, charged staff, meteorite
  compass, view cells; meteorites (mysterious cube) world generation; light detector behaviour.
Textures: quartz glass connected textures, memory card colour hash, portable cell housing tint, ME chest cell
  state lights, crafting unit light overlays.
Buses (automation with other mods, priority): import bus and storage bus read a container's raw slots (no sides:
  a furnace's input/fuel get pulled); must use the face's accessible slots (vanilla WorldlyContainer rules,
  mod containers' ModContainer faces/outputs). Import/storage on AE2's interface (sub-networks) and pattern
  provider. Cards on buses: fuzzy, inverter, redstone (modes), capacity (1/5/9 filter slots), crafting card
  (export bus crafts what's missing), export scheduling (default/round robin/random). Fluid import/export/
  storage buses over `native_block_entity_fluids` tanks and cauldrons. Other mods' energy: no Horizon API.

## User report 2026-11-03 ~12:00 (screenshots 11.43-11.55) — work list
1. Pylons: multiblock column looks (done in 558d77/8325924 builds, verify in game).
2. Cables: a cable with one connection must show its other side as an end; smart cable connection textures look
   wrong; P2P tunnel's stub doesn't reach the cable; P2P model looks odd.
3. Liquids drawn as buckets: show the animated liquid texture (terminals, plans, buses...).
4. Drive cells in the ME drive not 1:1 (compare with AE2's drive cell models/LEDs).
5. Wrench: no turning a networked block to a side without a cable/network block behind it.
6. Parts placed on a cable face the player (back on the cable); tunnels need no extra cable under them.
7. Molecular assembler: 3x3 grid slot frames; cell workbench: config slot frames.
8. Quantum link formed look (done in new build; verify).
9. Condenser "-6144": data slots are 16-bit; numbers go through extras.
10. Shift over an encoded pattern shows its output (AE2).
11. Guide: W only for a hovered item (no looked-at block); Esc closes the guide (not back).
12. Energy level emitter UI (AE2's number entry, mode button) and it may only be placed on a cable.
13. Formation plane UI: AE2's (storage bus style, 63 config, cards).
14. Guide pages: every page checked; full markdown/MDX: paragraphs, **bold**, lists, tables, RecipeFor/Recipe,
    ItemGrid, CategoryIndex, SubPages, ConfigValue, images, scene annotations.
15. Crystal resonance generator points out of the clicked face (directional).
16. AE2 JEI integration: fluids as animated textures with a page each on how they are made/obtained, other
    mods' fluids too.
Then: tests visible (user watches), then publish each mod.
17. Parts sized as their models (collision/outline per facing) — done (shapes); backs of terminals drawn
    (faces inset off the block edge) — done; buses/planes/tunnels without a built-in cable (`wired`) — done.
18. Terminals offline: whole screen drawn, grid dimmed with AE2's notice; first list sent at once — done.
19. Meteorites in world generation, 1:1: structure set random_spread spacing 32 / separation 8 / salt 124895654
    (Java Random from the world seed), biomes #ae2:has_meteorites, MeteoritePlacer (sky stone sphere, crater,
    fallout by biome), mysterious cube with its loot. Hook: native_chunk_load(new) then place on a later tick.
20. Storage cells in the hand do nothing; sneak-use on an empty cell takes it apart (done).
21. Smart/dense cable lines by each cable's own channels (done: per-cable pathing counts, 9 stages).

## Round 12 (2026-11-03): done and left
Done: crystal resonance generator (six facings, AE2 box, joins only through its back); no-UI machines pass the
click to the held item (growth accelerator, CRG, quantum ring, pylon); parts placed by the real clicked face;
bus/plane/tunnel cable stub is solid; cable interface / pattern provider / energy acceptor and the storage and
conversion monitors place and work (configure, lock, store, punch to take, item on the screen); IO port rewritten
to IOPortBlockEntity (shared 256..2048/tick budget, partitions, fullness + redstone modes, AE2 icons/tooltips;
1.5M items each way at AE2's rate, no lag); ME chest top lights and chest.png screen; vibration chamber screen;
off-panel toolbar buttons go over ae2:ui (workbench partition/clear now work) with tooltips.
Left:
- ME chest: clicking its top should open the inserted cell's own terminal (ChestBlock.onActivated).
- Monitors: the amount text under the item (no world text API yet).
- Cell workbench: upgrade panel, fuzzy and copy-mode buttons.
- Part stub hitbox is there even when the part isn't wired (shapes can key only one template property).
- Guide: scenes as real 3D structures, drag to rotate (HudBatch quad_3d + depth_since; per-state item models).

## Round 13 (2026-11-03): done and left
Done: parts on the shulker-box template (no see-through floor); planes 1:1 (PlaneBakedModel inset plate, joins
same-kind neighbours, transition_plane_back); part status rings off/has_channel + powered for all parts; screens
lit (light_emission) when powered, same tint off; formed crafting storage/accelerator lights (glow powered);
vibration chamber burning look and fuel via Horizon's burn time (it never burned before); level emitter back cap;
cable bus: bus/plane/tunnel hold the neighbour's own cable (type, colour) and join on every side but their front,
other parts only through a back cable, cables draw no arm toward a part's front; storage monitor amount text
(AE2 slot format, own pixel font) and full number on click; terminal clicks per MEStorageMenu (shift-right one,
shift-scroll in/out, space fills the inventory, shift-left stack) with gives delivered from the server tick;
cell workbench upgrade panel (cell cards, CellUpgrades/fuzzy now used by cells), fuzzy and copy-mode buttons,
own config pane; ME chest top opens its cell's own terminal.
Left:
- Guide: real 3D scenes, drag to rotate (HudBatch quad_3d + depth_since; per-state item models from the guide's
  structures).
- Middle-click creative duplicate on terminals (Horizon gives native_screen_click only left/right).
- Parts: has-channel vs powered-without-channel ring (both shown as has_channel when powered).

## Round 14 (2026-11-03): done and left
Jade: v2 overlay (tooltip frame, harvest tool, item rows, Jade's own progress arrow, hearts/armour); entity
targets (mobs, item frames, paintings, armour stands, dropped items); horse/llama stats, villager profession
and level, zombie cure time, next egg/scute, wolf armour, status effects; universal fluid bars for mod tanks
and cauldrons (Show Details adds the capacity); `§#RRGGBB` text colours.
Jade-AE2: AE2's igtooltip providers 1:1 (grid node state in grey, smart-cable channels and channel errors,
P2P state/frequency in AE colours/carried channels, charger contents and charge, crafting monitor, stored
power per block, pattern provider lock reason, monitors' Showing/Locked, plane enchantments, CRG Suppressed).
AE2: CRG suppression (one generator per grid); powered items (battery, charger for every chargeable item and
every charger recipe, green bar, "Stored Energy" tooltip, wireless terminal drain/range/low power, entropy
manipulator 1600 AE, applicator 100 AE); pattern provider screen 1:1 (pattern_provider.png, return
inventory, blocking mode, five lock-crafting modes with redstone/result unlocking, pattern access visibility,
priority tab); annihilation plane keeps the placed item's enchantments (fortune/silk drops, efficiency and
unbreaking power); charged staff (300 AE, 6 magic damage, sparks); tiny TNT (flint and steel or redstone,
80-tick fuse, AE2's crater formula, 6 damage, entangles singularities); `/ae2 channelmode` (infinite, default,
x2..x4, saved); meteorite compass (3D model, needle toward the nearest meteorite, AE2's ring search);
matter cannon (ammo cell, nugget/matter ball weights, speed cards, penetration through mobs and blocks,
paint balls dye sheep); colour applicator as AE2's paint cell (dyes/paint balls/snowballs, cycles, recolours
cables and vanilla colour families); creative middle-click duplicate on terminals; guide scenes in 3D,
drag to rotate.
Left:
- Wireless crafting terminal's crafting grid (the wireless screen has no real slots).
- Facades (and JEI's facade plugin); paint splotch blocks from paint balls.
- JEI ghost-ingredient dragging into config slots (Horizon's panel reports no drags).
- Compass needle in the hand (only the GUI icon turns).
- Annihilation plane enchanting in tables/anvils (the plane item's template isn't enchantable).

## Round 15 (2026-11-03): done and left
AE2: facades (414 eligible blocks, AE2's anchor recipe, facade plate item models, placing swaps the cable to a
beacon-template twin that carries the facades and draws the block's six faces, sneak with empty hand/wrench
removes them, Jade names "ME Facade - <block>"); paint splotches from matter-cannon paint balls (skull-template
paint block, per-face dots in the AE colour, lumen paint lights 12/15, snowballs clean, placing a block into it replaces it
as AE2's replaceable paint block does); tiny TNT moved to a block-entity template so redstone primes it; wireless crafting terminal's
3x3 grid, result and cursor (grid saved on the item, ingredients pulled from the network first); crafting terminal
screens on crafting.png (terminalStyle rows, crafting header/footer); the grid's Store/Take Items buttons on both
crafting terminals; meteorite compass needle in the hand, frames and GUI (item_model swapped to one of 16 full
compass models every 4 ticks); part rings show powered-without-channel; guide scenes use the real block states
(smart cables join controllers/drives/parts with their channel lights, buses get their own cable stub, drive
cells by fullness, colours right); Jade config page (Options > Mods: 12 settings, 7 rebindable keys, toggle/hold,
narrate, recipe/uses lookups through JEI), add-on listings for ae2_jei and jade_ae2; annihilation plane
enchanting in tables and anvils (enchantability 10 as a default component, the digger tags, unbreaking left out
of the table's offers by a cancellable hook on Enchantments::available_results, as AE2 keeps it to the anvil); JEI ghost ingredients (drag from JEI's list or a recipe onto bus,
interface, level emitter, cell workbench and pattern encoder config slots, targets highlighted as JEI does, the search box
losing focus on the press as JEI's does); creative middle-click on block terminals puts a full stack on the cursor
(CREATIVE_DUPLICATE as a menu button; a hook on lan_station_button says who clicked and their mode, since Horizon has
its player list out of the game during a button); terminal cells are widgets (narrator, horizon-ctl ui/click); Jade's recipe/uses lookups open the
inventory screen in creative too. Verified in game: stash buttons, compass in hand, plane enchanting (table, anvil),
ghost drags on all five screens, Jade keys in survival and creative, interface stock amounts up and down.
Both wireless terminals keep the player's cursor on the server as AE2's MEStorageMenu does: left takes a stack,
right half, onto the cursor; a click on the grid while holding puts it in (right: one), inventory clicks pick up
and put down, creative middle-click duplicates onto it, closing returns it (verified in game).
Facade cables collide as AE2's CableBusContainer does (core and joined arms of the cable's span, a two-pixel plate
per facade): 768 colourless twins `hz_fcb<core>_<n>` on the skull's template, each with sixteen shapes keyed on its
`rotation` (look n*16+rotation = arms | facades << 6); the twin's block entity holds the cable, its lights and the
facades, the network reads twins through `block_id` (as `ae2:hz_fcable_<cable>`), the cable is drawn with
native_draw_block, breaking drops the cable and its facades, the applicator recolours the cable under them.
Verified in game: placing and removing facades, arms to neighbours, a networked smart cable keeping the network,
walking into a glass cable with a north facade stops at its core (z + 0.925, not the block's face), drops.
(Hooking Horizon's collision query was tried first and isn't possible: generic or trait functions only.)
Cables with facades saved by older builds (`hz_fcable_<cable>`) stay registered and become shaped twins on their first
tick, facades, arms and lights kept. Jade names a cable with facades as AE2's PartHostTooltips does: the facade under
the crosshair ("Cable Facade - Stone"), else the cable. Verified in game, with the AE2 JEI layer's Inscriber category
and "+" transfer into a crafting terminal.
LAN, with a second game as guest (creative): terminals open and list items, the guest's middle-click, shift-click and
stash act for the guest even with the host on the same terminal (a hook on lan_station_button says who clicked), JEI
drags, the wireless cursor, facade cables' looks, Jade names and collision. One guest's first opens of the crafting
terminal did nothing in one session; it never recurred across restarts, in any order tried, cause unknown.
JEI drags go only to AE2's FakeSlots, as AE2's GhostIngredientHandler does: the pattern encoding terminal yes, the plain
and crafting terminals have none (real AE2 neither), locked slots no.
JEI on the wireless terminals: their mod screens are told as inventory screens only while the panel draws (RETURN on
Game::hud) or takes clicks, typing and keys (from the frame), with its width set past the side column; "+" fills the
wireless crafting grid. JEI's and the guide's keys look up a terminal's items under the pointer (ClickableIngredient).
The panel keeps clear of AE2's side columns (getGuiExtraAreas), and a press on them is no longer taken for the panel's.
Verified in game: the panel, search, recipes, "+", R and U, upgrade slot clicks, drops on slots a card unlocks.
Audit against AE2's jar: every item and block in its language file is in the port but the nine debug_* tools (gated by
unsupportedDeveloperTools, off by default, not gameplay: left out on purpose). Recipes left out: the matter cannon's ammo
(in the native table, same weights), facade (the generated recipes). The five *_fluix_clean cable recipes were missing
(fabric:difference): added, a coloured cable with a water bucket or snowball makes fluix, verified in game.
Left:
- JEI on the creative inventory: Horizon's panel limit, in the JEI bundle's area, not AE2's.
- Dedicated servers: Horizon doesn't build native mods for them yet.

## Round 16: JEI window, collection pages, job forecast, Jade audit, performance

JEI (all of it in `mixins/ae2-jei`; the original JEI bundle is untouched):
- A centred recipe window (`window.rs`) replaces recipes in the side panel: tabs, category and page bars, the catalyst
  column, cards, a tint behind the screen and the panel. Esc and E close it, Backspace goes back, the wheel turns pages.
- Every item has a page (`collection.rs`): loot tables give block, mob, chest, archaeology, fishing, bartering and
  shearing sources with exact chances worked out from the tables (not rolled); a thread reads them once, about 2 s,
  when a screen first opens. Audited over all items: none left without a page.
- World Gen: ore graphs (share of blocks per Y) from the placed features, calibrated against a chunk scan (a vein places
  about 0.6 of its size; stone gives way to air above Y 40). Natural blocks by world and biome, with grass, netherrack
  and end stone as the world icons. Structure blocks from template palettes, plus a hand list for structures built in code.
- Mob Drops: the mob in 3D, turned by dragging, spawn biomes, light level, experience, all its drops with chances.
- Villager Trades: profession, work station, level, trade, on an item's recipes and uses.
- AE2's presses: Mysterious Cube, meteorites, spawn rate, the compass.
- The panel and window work on the creative screen too, including R and U over its grid.

Auto-crafting: `CraftingCpu::forecast` plays the job forward from what is connected now (providers online with the
network's channels, P2P and channel mode as they stand, speed cards, measured machine and assembler times, the CPU's
pushes per three ticks and its co-processors). Worked out at most once a second, only while a status screen is open.
Molecular assemblers now take one craft at a time, as AE2's (they were not busy before, so jobs ran too fast).

Measured on real jobs (forecast at each moment against the actual finish), with the status poll in the loop:
two-level crafting 400 sticks in 50.6 s: worst 2.1%, average 1.0%; with speed cards in 39.8 s: average 1.5%, worst 5.5%
in the first seconds, before the assembler's cycle is measured, then within about 2%; furnace 10 stone in 100.7 s: worst
1.0%, average 0.6%. Not tested: channel modes other than the default, P2P between provider and machine, several CPUs at
once, or modded machines (they start from a 40 tick guess until measured). A forecast cannot know about a machine
the player breaks or an input that runs out mid-job.

Jade: mob timers (growing, breeding, next egg or scute, cure, restock) and horse stats now come from the server: the
client's copy of a mob does not have them (a baby's growing time read 0.1 s). Still not ported from Jade's options:
themes, alpha and text background, profiles, the ignore list, boss bar overlap, TTS modes, perspective mode, reach
distance, display item position, animation, flip main hand, item mod name, most plugin sub-options (harvest tool,
storage styles and amounts), relative coordinates, explosion resistance and jump, speed, ignite and burn in block
properties, the sniffer's dig cooldown, and accessibility-only providers. Those are differences from Jade.

Performance, same world and same build, one 30 s sample each, all mods on against off in Options > Mods: 176 fps
against 178 fps, tick 10.5 ms against 10.3 ms (P99 14.9 against 21.5 ms), both at 20 TPS: the differences are about the
size of the noise. An AE2 base idle: 3.2 ms a tick, with a job running 3.3 ms. No comparison with Java was made.

## Round 17 (2026-11-04): AE2's screens and settings against the jar

Built as `native-c9d474d-69a66f904089`, tested in my offscreen game unless noted.

Done and tested:
- Crafting Status and Crafting Plan rebuilt on AE2's own textures and layout; the plan's CPU button (Automatic, or a
  CPU) with CraftingService's choice; the status screen on crafting blocks with the CPU's auto-selection mode.
- Crafting monitor: the job's item and what is left on the block, its lights in the monitor's colour.
- Molecular assembler: the animated power light, sparks while crafting, the item still in the glass.
- Terminal: the six toolbar buttons with AE2's icons, the crafting status tab, the terminal style resizing live, the
  settings screen, pinned items, the search syntax (`@ # $ * |`), expressions in number fields.
- Upgrade Toolbelt under the upgrades with a network tool; Compatible Upgrades on the panel, from InitUpgrades.
- Crafting cards on the interface and level emitter (including emit-to-craft); craft only and fuzzy on the export bus;
  fuzzy modes on buses, emitter and interface.
- Storage bus: clear, partition, access mode, filtered operations, report inaccessible, what it's attached to.
- Formation plane drop mode; annihilation plane on fluids and on blocks with block entities.
- Pattern encoding: substitutions, cycle outputs, 81 inputs and 27 outputs with the scrollbar.
- Pattern Access Terminal's display modes and output search. Spatial IO port's power, efficiency and cost.
- Vibration chamber's cards and its speed following the network; inscriber's speeds, power and Auto-Export.
- Fluids count 1000 mB a bucket everywhere. Fluid amounts are typed in buckets to the millibucket (0.01 B). Patterns
  take fluids (right-click with a bucket), providers fill tanks, and the CPU collects exact amounts: tested with
  10 mB water in, 5 mB lava out, 0.015 B requested (30 mB pushed, 15 mB collected).
- Finished-job toast. Measured: 20 TPS, tick 3.3 ms idle, 2.0 ms during a job (different views; no Java comparison).

Left, not 1:1:
- Inscriber: Automation Access Mode (Horizon's container faces are fixed per block) and input stack size.
- Fluid Substitutions on crafting patterns; fluids in the interface's stock.
- Storage bus onto an ME Interface (subnetworks); parts of a plane's fluid placement.
- Spatial: cells hold their contents instead of a dimension, no Serial, blocks with block entities stay behind.
- Spatial Anchor: Horizon has no chunk forcing, so no chunk loading or overlay.
- Transparent facades with the network tool (block entity drawing has no alpha here, and facades are block models).
- Cell tooltips list contents as words, not icons. Wireless slot hints and Invalid Singularity text aren't there.
- ETA checked with several CPUs on one machine, speed cards and furnaces; not with P2P between provider and machine,
  other channel modes, or modded machines.
- Dedicated servers: Horizon doesn't build native mods for them, so "server" means a LAN host.

## Round 18 (2026-11-04): the gaps Round 17 left

Tested in my offscreen game; the interface and the terminal also on a LAN guest.

Done and tested:
- Storage bus on an ME Interface: with no config, the interface's whole network, both ways and through the bus'
  filter (64 diamonds on the sub-network seen, taken and stored from the main terminal); with a config, its own
  storage. A pattern provider beside another network's interface pushes into that network.
- ME Interface fluids: config by bucket, stock to the mB (0.25 B tested), back to the network when lowered, four
  buckets a slot, a bucket on the stored fluid.
- Fluid Substitutions: the button, default on, the green cells, and the job (1000 mB of water for a water bucket).
- Crafting patterns give back what recipes leave: cake returned its three buckets.
- Inscriber: AE2's filters for hoppers (coal refused), Automation Access Mode, Input Slot Stack Size, the press cycle
  and its time, plates either way up, the Name Press.
- Fuzzy Cards at work on the export bus, level emitter and interface, with a worn tool.
- Formation plane: pours a bucket as a source, waterlogs, filters preferred as AE2's. Empty slots show AE2's icons.
- Items in water: all the `ae2:transform` water recipes (fluix crystals and certus tested). Pearls count in the
  singularity blast.
- Crafting jobs survive quitting: 200 planks, quit partway, reloaded, exactly 200 at the end.
- Molecular assembler on its own: pattern slot, grid hoppers fill by the pattern, result pushed out, real screen.
  Assembler times by AE2's table (10, 8, 6, 5, 4, 2 ticks), charged AE per craft. Its result unlocks a provider.
- Wrench: sneak picks a machine up with its settings and contents, placing restores them; turns about the clicked
  face; sets a pattern provider's push direction (with AE2's oriented model), which pushes then obey, round robin.
- Memory card: all settings and cards, partial restore on another machine, clearing, AE2's messages.
- Upgrade cards: sneak-use installs one with AE2's limits; portable cells, cannon and applicator take cards in the
  workbench; Energy Cards raise capacity; taking a cell apart gives its cards back.
- P2P: only over a running grid, even split, fluid tunnels. Growth accelerator on crops, saplings and the rest of
  its tag. Crank on the inscriber and accelerator. Controller opens the network status. Interface priority saved.
- Storage buses reach sky chests and tanks. Quantum link screen as the jar's. Invalid Singularity tooltip.

Left from Round 17's list, still not 1:1:
- Inscriber with separate sides: hoppers can still only take the result (and from any side); `can_take` holds a macro
  that horizon-mixins won't patch. Hand-placed stacks aren't held to the stack size setting.
- Water poured by a plane in the Nether evaporates silently.

Also done after the audit: hotkeys (wireless terminal tested from any slot), Pattern Access Terminal's style button,
monitors show a bucket's fluid, knife takes any metal ingot, energy cells keep their share of the charge as items,
entropy manipulator (TNT, Tiny TNT, smelting into drops, fire, alight on hit), fluix tools' fortune and looting
(15 coal from 12 ores), dispensers (Tiny TNT, entropy manipulator), climbable cable anchor, applicator on the wheel,
provider fluid returns, light P2P through a light block before the output, wireless terminals in the workbench,
providers and buses on AE2's own machines (inscriber tested), P2P as a provider's target.
Estimate through item P2P with a hopper return: mean 3.3%, worst 4.3% over 49 s. Channel modes change no timings.
Measured on this build: 19.98 ticks/s, tick 2.43 ms average, P99 6.20 ms; no Java comparison.

Open, by how much a player would notice:
- Your call: a provider's machine output comes back without a return path when the machine is beside it (the CPU
  reaches into the machine, and into a chest target). AE2 needs a hopper or pipe into a provider or an interface.
- One part per block: no cable bus holding six parts. Parts are separate blocks, facades on cables only.
- FE: Energy Acceptor, FE P2P, powered items: API 142 and 307, on a newer Horizon than this revision.
- P2P: no extraction through an output, no power tax. Wireless terminals' cards go in the workbench, no panel.
- Portable cell: right-clicking items into it. Applicator pick colour. Network tool tooltip. Matter cannon in a
  dispenser. Sky stone break speed. Provider send list. Interface priority only orders storage.
- Inscriber separate sides: hoppers can't take presses out; hand-placed stacks ignore the stack size setting.

## Round 19 (2026-11-04): all four mods on Horizon 6475f62 (API 359)

Revision pins: Horizon `6475f62f3c834b7489e1816bf47f47ea5eb2b879`, hooks 359, horizon-mixins `e8cd0aa38cff...`. Each
mod's manifest is `manifests/<mod>.json`; `native-build/<mod>` holds it and a junction to `mixins/<mod>`. Build with
`horizon-launcher native-build "<native-build>" --allow-version-mismatch --trust-native-code`; the switch is there for
the untouched JEI bundle, whose manifest names its own older revision.

API changes taken:
- Every injection the catalog lists now uses `hook` + `hook_version`: ae2 57 named (11 raw), ae2-jei 8 (5), jade 9 (2),
  jade-ae2 3 (0). Raw targets stay pinned to this revision.
- Screen hooks fire on the title screens since API 326, so they return when there's no player. `Ingredient`,
  `PrimedTnt`, `key_pressed(key, raw)`, `NativeScreen.own_background` and the `ModBlockEntity` revision wrapper followed.
- Energy (API 142, 307): Energy Acceptor, Controller, Charger and Inscriber take FE at 1 FE = 2 AE, FE P2P moves it,
  powered items turn `horizon_energy` into their AE charge.
- Slot rules (API 130, `menu_slot_rules`): AE2's RestrictedInputSlot and OutputSlot on every menu. Hoppers follow the
  same rules through `Containers::can_place`. A card a machine doesn't take, or past its limit, goes back to the
  player; so does what doesn't burn in a vibration chamber, because the rule hook has no fuel table.
- 26.3 loot tables rewritten by the content generator; block hardness as the jar's.

Bugs found by this round's tests:
- A guest's `/jade` ran as the host, hiding the host's overlay; a guest's `/ae2` ran with no reply. Below a command's
  level, or with no mod taking it, the server's dispatcher reaches the client `command` hook inside the guest's swap.
  That hook now hands it to `perform_command` for the acting player.
- Clicking a view cell slot threw the item on the ground: Horizon counts a click past the window as outside even on a
  slot. `clicked_outside` now knows AE2's slots, toolbar and side panels (AEBaseScreen.hasClickedOutside).
- After a stack went into a portable cell the cursor kept drawing it until the next click (nothing was duplicated).

Tested on the build from these sources unless noted, in my offscreen game:
- 30 slot rule checks by real clicks, shift-clicks and hoppers. FE: 50 000 FE into 100 000 AE; staff 1 000 FE into
  2 000 AE. FE P2P moved 30 000 FE to the acceptor; the charge didn't reach the cell behind it in that test.
- Water transforms, fuzzy cards, sub-network take and store, fluid substitution (exactly 1000 mB, no buckets), cake's
  buckets, a job quit at 188 of 256 planks that finished at exactly 256, applicator wheel and pick colour.
- LAN guest: terminal, wireless terminal, JEI recipe window, Jade on AE2 blocks, `/jade` on their own overlay only, `/ae2`
  refused at level 2. Title screens: keys and clicks, no panic.
- 20.00 ticks/s, tick 1.86 ms average, P99 3.20 ms, 280 fps on the test world. No Java comparison was run.

Not tested on this revision: looting on fluix tools, ETA on modded machines. FE into a real FE source's cable: no other
mod here makes FE, so tests set the buffers directly.
Still open: the list at the end of Round 18, without FE, portable cell clicks, pick colour, network tool tooltip and sky
stone break speed, which are done.

## Round 20 (2026-11-05): independent audits of all four mods against their Java

Three audits read the decompiled Java against the Rust and listed what was missing. What they found, and Round 19's own
list, closed here unless noted.

AE2:
- Hoppers: under a P2P output they pull what the input faces, at the hopper's own timing, taxed; an input gives nothing.
  Under an inscriber with separate sides, the bottom press and not the result.
- Pattern provider send list: AE2's `adapterAcceptsAll`, what doesn't fit waits, the provider is busy, it drops on break.
- Machine output needs a return path, as in AE2: a hopper or pipe into a provider or interface. The CPU no longer reaches
  into the machine. `craftingCpuPullsFromMachines` in `config/ae2/common.json` turns the old way on.
- Interface priority between interfaces on one network. Wireless terminals' upgrade panel. Cannon and applicator in
  dispensers. Stack size on hand-placed inscriber stacks. P2P tunnels' idle power. Creative cell's real capacity.
- Spatial cells carry block entities and mobs, with a serial (SP-0001). The Spatial Anchor force-loads its grid's chunks,
  takes AE2's power, releases them when removed by any means, and has its overlay button (a frame, see below).
- Cell tooltip with icons (API 68). Status lights, portable housings and the applicator's tip through item tints, written
  into `custom_model_data`. Portable cells and memory cards take the vanilla dye recipe; a cauldron washes them.
- 408 advancements from the jar, on 26.3's format. AE2's own five triggers go through `/advancement`.
- Tiny TNT visible while lit. Comparators on inventories and energy cell fill. Memory card copies patterns and spends
  blanks. Power unit toggle. Ambient particles. Portable and crafting unit disassembly with AE2's message and cards back.
  Network status rates, drain and tooltips. Interface wrench button. Plane's 128-entity limit. 25 AE per grid node.
- `config/ae2/common.json` and `client.json`, AE2's keys and defaults, `/ae2 reloadconfig`. Old settings migrated once.

Jade: FE lines, progress, contents of mod blocks and chested mobs, locked and unlooted containers, display entities, pots
and heads, loot table lines, accessibility master switch and `jade.access.*` wording, profile keys and presets, toasts,
narration, camouflage (infested, trapped), harvest tool tiers and shears, comparator and sculk power, sniffer, allay,
spawner minecart, trial spawner, wax on signs, shelves, pet armour, real armour and absorption, more `registry_name`.
Jade's AE2 plugin: its toggles and the debug card lines.
AE2's JEI: "+" state with marks and tooltips, machine recipes refused, Ctrl-click craft missing, encoder marks, facade
recipes, terminal catalysts, U on a catalyst, three certus pages, API attunements, inscriber click area, wireless search sync.

Not possible as a Horizon mod today, by what the API and source show:
- A spatial dimension (no mod levels), so players don't enter cells: entities in the volume go into the cell's data.
- Fluix Researcher: professions are compiled-in tables (render, brain, network ids) with no hook.
- Light P2P uses a light block in front of the tunnel.

Jade also has its themes (the jar's seven, and packs'), the overlay's drag-to-place screen, and Home, Mod, Plugins and
Profiles config screens with search, sliders, reset, a live preview and AE2's section. The confirm screen's crafting plan runs
on its own thread and the job starts from that plan; a job asked for another way still plans on the server thread.

A correction: an earlier version of this list called see-through drawing impossible. `world_drawn` gives a mod the wgpu
pass, so `ae2_draw.rs` draws it: the anchor's translucent walls, CABLE_VIEW's textured see-through facades with a network
tool in hand, and the placement preview.

Done after the audits, on `native-6475f62-5f9a5a693e90`: Jade's card layout, arrow-key focus, mouse binds, config text in
other languages, HSL theme colours. AE2's `showPlacementPreview`, `NETWORK_BOOTING`, the nested P2P error (no controller),
power buffers on charger, inscriber and chest, channel drain charged, `allowBlockEntities`. JEI's facades in the panel and
Ctrl-"+" through the confirm dialog.

Tested on that build in my offscreen game:
- Slots, wireless panel, spatial there and back, P2P hoppers, send list, interface priority, anchor, overlay, looting, comparators,
  memory card, disassembly, dispensers, config reload: all pass. Quartz fiber: power joined, storage apart; unjoined, no power.
- By screenshot: translucent overlay, facade preview, placed facade, see-through with a network tool, bus preview, Jade cards.
- Ctrl-"+" on a crafting table with no planks: the plan dialog (To Craft 4, Available 1), Start, 1 log into 4 planks.
- Nested tunnels without a controller: the drive goes to Device Missing Channel; a single tunnel stays online.
- 983 fps, 20.01 ticks/s, tick 1.70 ms average, P99 3.90 ms on the test world, GPU pass included. No Java comparison was run.

Not confirmed by a test: `Network Booting` (too brief for a screenshot), the nested error's own text line, chest and inscriber
buffers, `allowBlockEntities`, mouse binds, non-English config text, HSL colours.

The dedicated-server engine builds with all five mods (`--server-output`). It hasn't been run: that needs the Minecraft EULA
accepted, which is the owner's to do.

## Round 21 (2026-11-05): the cable bus, and what Round 20 left

Round 20 listed one part per block as not attempted. It is done now, without rekeying AE2's logic.

The cell is a facade twin (`ae2:hz_fcb*`), whose block entity lists its parts (`parts`: side to id, state, and what its client
draws). Each part is a block entity of its own under a position past the top of the world: same x and z, y `1024 + (host y + 64)
* 6 + side`. Those stay on the server and are saved with their chunk by the engine as they are. Clients never see them; they draw
from the twin's tag. `add` and `face_toward` step from the host's cell, so a part's target is the real neighbour, and the
device code is the code of a part with a block of its own.

Engine hooks, raw and pinned to this revision: `native_block`, `native_block_property`, `native_set_block_property_with`,
`native_set_block`, `native_block_entity_sync`, `native_neighbor_signal`, `native_update_signal_neighbors`, `native_insert_item`,
`mod_menu_at`, `native_hidden_slots`, `open_native_screen`, `remote_block_reacts`, `predict_break`, `outline_boxes`,
`hopper_eject`, `lan::menu::shown`, `lan::menu::Slots::still_valid`. Named: `is_signal_source`, plus new bindings on
`destroy_block`, `block_signal` and `block_highlight`.

What it does, as CableBusContainer does:
- A part on a cable goes into the cable's block, on the clicked side. The side's arm closes. A taken side, a sneak on one, or
  anything but a cable: a block of its own beside, the old way, so old worlds keep working.
- Aiming and the outline are each part's own shape. Standalone shapes are made for a block beside a cable (a plate at its back, a
  bus with a stub), so `part_fit` moves and trims them; the model moves with them.
- Each part has its own screen and reach. Tools, memory cards, wrenches, monitors and Jade act on the part aimed at. A facade and
  the applicator act on the cell. A network tool selects through facades.
- Breaking takes what the crosshair is on, a part or a facade, and the cell stays; a wrench the same, into the inventory. The cable
  takes the rest with it. A cell removed any other way (explosion, command, plane) drops its parts at the next sweep.
- Redstone goes out of an emitter's own side only. Neighbours (storage buses, providers, P2P outputs, hoppers) reach the part on
  the side facing them. Toggle buses, quartz fibers and ME tunnels join what they should. Spatial cells carry parts.

Differences that remain, and why (all but the last line fixed since: see Round 22):
- Collision: a twin's shape has twelve bits, so a part collides as a rod (arm) or a plate (facade). Aiming is exact.
- A powered panel's light is a light block in the air in front (the twin's states are spent on its shape). In front of a wall, none.
- Hosted parts are drawn as block entities: not past 64 blocks (`VIEW_DISTANCE`, in a generic the mixin tool refuses), and at
  some frame cost. 99 hosted parts in view: 443 fps against about 800-900 without, 20.02 ticks/s.
- FE and fluids from other mods' pipes don't reach a hosted part: the engine finds those by block state. AE2's own buses do reach
  items in hosted interfaces, and hoppers reach hosted parts.
- A part placed on anything but a cable is its own block, and a second part on it goes beside rather than into that block.
- Positions assume worlds no taller than -64 to 319, and server tables aren't per dimension (as the port's other tables aren't).

Also closed:
- Sneak with a wrench on a cable with facades lost both and put an air item in the inventory.
- Import and export buses tick as AE2's (`TickRates`: 5 to 40 and 5 to 60, faster by 2 on work, slower by 1 without). 40 items took
  14.8 s against 13.4 s worked out from Java's rates, which is the test's polling.
- Jade's overlay and AE2's lines use the jars' language keys: 170 of 185 `jade.*`, 28 of 28 `waila.ae2.*`. Time reads as Java's
  (`3m 2s`). Item words go through the language files. Checked in French. The rest of `jade.*` have no such line in the port.
- Jade's 53 card icons, from each provider's `getConfigIcon`.
- JEI `requestedSlots`: nothing to port. `CraftConfirmMenu` writes that field and never reads it.
- Charger and inscriber buffers tested: a crank fills them, an empty charger holds its crystal, a full one charges.

Still not as Java (overtaken: see Round 22, which found ways for most of these):
- A spatial dimension (no mod levels), so players don't enter cells.
- Fluix Researcher: professions are compiled-in tables.
- Light P2P uses a light block in front of the tunnel.
- `jade:max_position_deviation` is a setting, as `GameRules` is a fixed table.

Tested on `native-6475f62-cd2e4e69f140`, in my offscreen game, none of it panicking:
- Cable bus: placing on each side, a cell's list, four kinds on a cell, working network I/O, each part's screen, facade over a part,
  wrench and breaking one thing at a time, a save and reload, a cell removed by command, hoppers, panel light, plane links, an emitter
  lighting only its own side, ME tunnels, storage bus on a hosted interface, fiber, toggle bus, monitor, spatial there and back.
- A LAN guest: breaks a part, places one into the freed side, opens that part's screen; their world agrees.
- Everything from Rounds 19 and 20 again: slots, water and fuzzy, P2P, inscriber, cannon, send list, wireless panel, spatial, anchor,
  tooltip, overlay, looting, status lights, TNT, comparators, memory card, disassembly, dispensers, config, JEI's Ctrl-"+".
- By screenshot: a full cell, the outline on one part, a facade with a part in it, a network tool's view, Jade's cards with icons, Jade
  and AE2 lines in French, `Network Booting`, `Error: Nested P2P Tunnel` on a smart cable.
- 733 to 983 fps across runs (other work shared the machine), 20.00 ticks/s, tick 1.80 ms average, P99 2.60 ms on the test world. No
  Java comparison was run.

Not confirmed by a test: `allowBlockEntities`, Jade's mouse binds and HSL colours. `craftingCalculationTimePerTick` only reads on jobs
that don't come from the confirm screen, whose plan runs on its own thread. Nested P2P is worked out from stretches between tunnels,
an approximation. Crafting ETA on the P2P furnace rig averaged 1.1% off and reached 2.0%: close to the 99% asked for, not always within it.

The dedicated server had still never run at this point: see Round 22.

## Round 22 (2026-11-05): the last of Round 21's list, and networks per level

Corrections to Round 21 above. Collision, panel light, far drawing, FE into hosted parts and a part on a block that isn't a cable were
all fixed before this round's tests: exact shapes per position, light by block state (`hz_fcl{level}_{n}`), cells baked into the chunk
mesh, FE by `native_energy` hooks, and a cell with no cable. Its gamerule and Researcher lines no longer hold either. The dedicated
server has now run: the owner accepted the EULA in `test-server`.

Done:
- **Fluix Researcher.** A real profession through mixins on the compiled-in tables: job at a charger, Java's trades per level, level-ups,
  its look, its title. It stayed offerless at first because `Game::offers_from_set` finds a set in the world's tables and then rolls it
  from the resource root's, which have no mod's files. `researcher_offers.rs` rolls `ae2:` sets on the world's tables. That engine line
  would hit any data pack's new trade set too.
- **`jade:max_position_deviation`.** A real game rule: on the Edit Game Rules screen under Miscellaneous, range enforced, saved with the
  world, read by commands, completed on a guest's chat line.
- **Players in spatial cells.** Native mods can't register a level (arismoko/horizon#1042), so a cell with a player in its volume gets a
  plot in the End, as a teleport allows. `plot_swap`: the port pays, force-loads the plot, and once it is loaded does the whole swap in
  one tick. A live plot goes back into the cell first, the ordinary swap runs, and what it took goes out to the plot with matrix frame
  walls. Then the players teleport. A cell with nobody in it stays in its item as before, and costs nothing new.
  The band is x and z from 1,000,000, with a 65,536 margin: void terrain (`plot_void`), no far terrain (`plot_lod_*`), black sky, no End
  sky mesh, the game's music, and no "The End?" advancement.
- **Networks per level.** `server_tick` ran once, at the end of the levels' pass, against whichever level was last. With players in two
  dimensions the other level's devices weren't found and its networks went offline. This hit LAN and servers, not only plots. Now
  `level_in_place` (a hook on `Game::enter_level`) swaps `SERVER` and the part tables with the level, and every ticking level gets
  `level_tick`. Two networks at the same coordinates in two dimensions keep apart.
- **A world reopened with its player in a plot** opens the other levels (`plot_levels_open`), as Java does at startup, or the port that
  gets them out never loaded.
- **A crash in the cable bus bake.** A section gone all air returns from `mesh_reusing` before its light is padded, and the cell table is
  up to six frames behind, so the bake read empty light and the mesh workers panicked. Breaking or clearing the last cells of a section
  could take the game down. `bus_bake.rs` now skips an all-air section, and `airsec.py` covers it. Earlier "failures after the stress
  test" in Rounds 21 and 22 were this crash.
- **A new bus inherited an old bus's rate.** One placed where another stood idle started at 40 ticks a move, not from the middle of its
  range: 27 s for 40 items against 13.7 s. Rates now go with their bus.
- **Jade's reach.** Block aim was capped at 5 blocks, and Extended Reach replaced the range. Java's is `blockInteractionRange +
  extendedReach`, and falling blocks and dropped items use the entity range. A long-armed player now gets the tooltip.

Still not as Java:
- Inside a plot, `/tp`, `/data`, F3 and maps say `minecraft:the_end` and x past 1,000,000. `execute in ae2:spatial_storage` fails. Region
  files sit in the End's folder. This goes when #1042 lands.
- The plot has a black sky without `SpatialSkyRender`'s stars.
- A swap with a player or a live plot takes about half a second to a second while the plot loads. Java's plot is always there.
- The Researcher and the game rule hang on mixins into compiled-in tables, so a Horizon update can break them. #1043 tracks the API.
- `/forceload` acts on the level in place whatever `execute in` says. Plots work around it; anchors act on their own level anyway.
- Standalone tunnels from old worlds still light by a light block. New ones are hosted, and light by their cell.

Tested on `native-6475f62-d72af06acdd9`, in my offscreen game, with no panic on host or guest:
- Full regression in one unbroken run, every suite passing, 0 panics: cable bus, kinds, hopper, collision, light, bare cell, FE,
  tunnels, emptied section, stress, reload, Researcher, slots, regressions 1 to 3, config, JEI, plots and LAN.
- Hosted Light P2P: the output's cell follows the input's level, 0 to 15 and back. Fluid enters a hosted part through
  `native_fluid_insert`, the lookup another mod's pipe uses.
- Plot round trip on singleplayer, a LAN guest and the dedicated server, across a quit and reopen and across a server restart, with a
  chest's items, a mob and a block.
- Guest in the Nether: the Overworld bus keeps moving items, also with a second network at the same spot there.
- Dedicated server (these on `2fcf330dd44e`, the smoke test again on the final export): `/ae2 channelmode|chunklogger|tickmonitor|reloadconfig`, `/jade`, the game rule, from the console and from a client.
  Hosted parts, terminal, breaking, restart survival, Researcher trades at each level.
- Jade's rule on a dedicated server: at 21 a barrel 10 blocks away lists `3x Diamond`, at 0 the server sends nothing.
- 731 fps average (low 694), 20.01 ticks/s, tick 1.80 ms, P99 3.40 ms on the test world; 664 fps and 20.02 ticks/s with 50 cells in view,
  tick P99 60.5 ms in that last run against about 7 ms in the runs before it, unexplained.
  A build's first launch loads resources in about 70 s, later ones in about 9 s, at 150,311 block states. No Java comparison was run.

Not confirmed by a test: an actual pipe from another mod (no such mod exists to test with), Jade's mouse binds. Crafting ETA stays as Round 20: 1.1% off on average, 2.0% at worst.

The owner's PC rebooted six times since Oct 29 with bugcheck 0x124, a CPU machine check, including once during these tests. That is
hardware, not these mods, but builds and two games at once load it. Builds here now run with `CARGO_BUILD_JOBS=12`.

## Round 23 (2026-11-05): one AE2 for the registry

The registry gives an id one entry and one kind, and a content mod's id is its blocks' namespace, so the content mod and the native
mod couldn't share `ae2`. A native `.hmod` holding both isn't published yet, and the check reads one as content only. So:

- The native mod carries the content: `data/ae2-content.hmod`, written to `.horizon-cache` and passed to `content_root` as a dev mod
  (`me_embedded_content.rs`). An installed copy of the content wins.
- AE2 JEI Integration and the Jade plugin are now in this bundle (`int_jei_*`, `int_jade_*`), at their old priority 110. The small
  `optional` crate keeps each out of the build unless its mod's module is in the staged `game.rs`, so a player's next build follows what
  they have. Their own Mods page rows and switches are gone: they follow `ae2`, and JEI's and Jade's own switches still apply.

Tested on `native-6475f62-a7ce977ec518`: the full suite, 0 panics, with no separate content mod; LAN; a dedicated server with none.
The only failures were a missed click in the LAN rig's setup, right after a plot trip. AE2 alone, with JEI, with Jade and with both all
compile. AE2 alone was played: network, tunnels, spatial plots and Researcher trades pass, no JEI panel, and `/jade` is unknown. With
another game running, the old and merged builds measured alike (271 and 297-365 fps).

A cargo cache note: switching mod sets can leave a crate's old build in place, since the stage restores a file's mtime. Clearing
that crate's `.fingerprint` folder fixes it. `hz_jei_keys` being always in AE2 keeps players' builds clear of it.

## Round 24 (2026-11-05): multiplayer security

The registry's maintainers reported that any connected player could send `ae2:debug` and get items, or delete and write patterns. It
was true, and four independent reviews of everything a client can send, and of how items move, then found more. All fixed:

- **Commands and messages.** `debug_command` checks the sender's own permission level. A client's messages have a size and rate
  limit, its queues have caps, and they act only on a terminal the server still counts as open, in reach and in the same dimension
  (`viewer_checked`). A ghost item goes only to config slots, never into storage or a pattern slot.
- **The crafting terminal's result** is no longer a slot. Taking it (`CT_TAKE`, `CT_TAKE_ALL`) works out the recipe and uses the
  ingredients in one step. A real slot could be taken from partly, or emptied along with the grid, for free. Recipe remainders are kept.
- **Crafting.** A player's job starts only from the plan they were shown. Calculations have a step budget, a depth limit of 128, and
  their own thread with a 16 MiB stack.
- **Duplication.** A cell whose contents change outside a drive loses its cached inventory's id (`write_cell`, found through an IO port).
  The toolbelt's clicks apply in the inventory tick, with the tool. The workbench's slots and cell agree before each click, since a tick can
  hold several, and only the cell's cards leave with it, whether swapped or broken. Only the first storage bus on an inventory reaches it.
  A provider's trial push puts back everything it touched, a double chest's other half and a cauldron included.
- **Item loss.** What a player or storage can't take now goes to the network or the ground: job output with storage full, an export
  bus's refused items, a wireless cursor when the screen closes. Breaking a CPU's key block cancels its job and returns what it held.
- **Other players' modes.** A button is its clicker's, so nobody else's creative mode can be used. Spectators use no terminal.
- **Jade.** Command block text needs permission 2, a client can only narrow what it's shown, reach counts for everyone but the world's
  owner, and requests have a rate limit.

Tested on a dedicated server by a non-operator, with a test-only client that sends any bytes on any channel (it stays out of the repo).
Each attack has a control proving the message arrives and works when it should. The cell and workbench duplications were also run
on a server with those two fixes taken out, which gave 128 items from 64 and a second card on the first burst, so the tests can see them.

Not run with such a control: two buses on a chest, the double chest push and breaking a CPU each pass, but the old code wasn't run
against them. Job output with storage full and the cauldron trial were read through, not run.

A test note: rebuilding the real mods right after the test-only client can leave its code in the build (`987c24b3c171` did), because
the stage restores mtimes. Search a test build's client for `ae2raw.txt`. A false pass or fail also comes from a second such client
running, since both take the file.

Final build: `native-6475f62-9132faf6f134`. Its full suite passed with 0 panics, and AE2 alone, with Jade and with JEI compile.

Not explained: in two of the five full runs (`87319f01c37b`, and the first on this build), placing `ae2:terminal` into a cable cell did
nothing for that game's whole session, while other parts placed. The next test of it passed each time: alone in the same game
once, and in a whole new run on this build, which was the same source. Those were both a build's first session, but `9f9ecb5e98a2`'s
first passed. No cause was found in the placement code. A player would see a terminal that doesn't place until they restart, if it isn't the tests.

## Round 25 (2026-11-06): another port's notes

The author of a second AE2 port compared the two and sent 75 defects read from the code. Checked against this source, 68 held, 5 were
partly fixed and 2 fixed. The worst of them are fixed here, on `native-6475f62-6b04867df72c`. What's left of the notes stays open.

- **`give_or_drop`** (Round 24) lost what didn't fit while a player was being handled, because the hub and its player list are out of the game
  then. It asks for the acting player first.
- **Stored power** is now each store's own: a rebuild shares a grid's power out over its cells and controllers by capacity and saves it on
  each block as `internalCurrentPower`. A reload, a lost cable, a split and an unloaded chunk all keep it. A grid with no store holds it per node,
  unsaved. A creative grid's isn't real, and leaves its stores as they were. The energy cell's look is AE2's floor.
- **Latch:** a grid that lost power comes back after 30 ticks with power. It's judged once a second, so 40 in practice, and it goes down at the next look, not at once. Power is enough
  for a tick's draw, not a second's.
- **Moving items costs 1 AE an item** (a bucket), through a terminal, an interface's stock, an export bus and a wireless take. An import bus pays nothing. **Not done:** the IO port and
  the crafting terminal's refills. **Idle draw** adds each cell's drain, and the block table follows AE2.
- **Channels:** access points, anchors and crafting blocks need one, a CPU or a spatial structure's touching blocks share one, level emitters need none, ME tunnels and
  quantum bridges carry 32. **Closed sides:** a drive, charger or inscriber's front, all but an accelerator's front and back and an access point's back, and the condenser and workbench join nothing.
  **Controller cross** is a conflict. **Not done:** unformed crafting, bridge and pylon blocks still join; a real rig can run out of channels that it didn't before.
- **Storage order** is AE2's on both ways. **A filled cell** isn't taken into a cell. What a terminal refuses goes back to the player from its hidden slot.
- **Terminal:** a click sends what changed, not the list, so the scroll and search stay and an index keeps meaning its item. Entries with data draw with it and name their enchantments.
  **Not done:** the rest of a stack's tooltip (damage, a custom name).
- **Content:** all four presses from a cube, parts and other shulker-template blocks stack to 64, nothing burns (items on `minecraft:paper`), a pickaxe works on cables, parts and cells, and a matrix frame can't be mined.
- **Pistons** leave a network alone (`piston_move`), which also answers what happened to a cable cell's cable and facades: nothing now.
- **Machines:** the inscriber's 200 steps; a charger takes one item it can charge and keeps it until it's done; budding quartz on the random tick's rates, with facing and fluid rules; the crank's turn, its hits and its drop.

Tested with `mechanics.py` and `restart.py` on a dedicated server: charge kept over a real restart with the network running on it and no source, four presses, stacks, fuel, pistons, priority, filled cells, two swords apart,
crank and charger rules. Every attack suite passes on this server. **Read, not run:** budding rates (their time scale), idle draw numbers, the power cost's last fraction, and how an enchanted entry looks.
The full suite passes apart from three checks that come and go on this same build: `it lists the network`, `sneaking on a taken side` and `the fluix sword loots` (the last depends on chance and passed on repeat).

Tests changed to AE2's rules, each a rig that relied on what's now closed: a drive whose front faced its cable, a provider joined through an access point's side, more channels than one cable carries.

The terminal placement failure of Round 24 is no longer thought to be the terminal: an import bus failed the same way later. The failing runs had tick spikes of 44-77 ms against a click held 100 ms, so the tests now hold it 180 ms.
Unproven, but it's not specific to any part. Frame rates vary with what else runs: the owner had a game open for some of these runs.

## Round 26 (2026-11-06): Horizon 66f3981

The registry's check builds on the newest Nightly, where AE2 15.4.13 didn't compile: engine types it fills in had gained fields. `PrimedTnt` takes `rotation` and `block_state`, `Damage` takes
`attacker_species`, `OpenNativeScreen` takes `extra`, and TNT's `tick` takes the block movement properties. Pinned to Horizon `66f3981`, horizon-mixins `5a64af9`, as version 15.4.14.

AE2 alone compiled as `native-66f3981-dd348c92bfbf`. **It wasn't run**, at the owner's choice. The last build to pass the test suites is `native-6475f62-6b04867df72c`, on Horizon `6475f62`. With JEI or Jade on this Horizon it wasn't compiled either.
The registry log's `ItemRenderState` and `title::Button` errors aren't in this source, and didn't come up in this compile: nothing was needed for them.
