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
- One part per block. Every device here is a block with its own position, entity and menu; a cable bus would need all of
  AE2's logic rekeyed from position to position-and-side. Large, not attempted.
- A spatial dimension (no mod levels), so players don't enter cells: entities in the volume go into the cell's data.
- Fluix Researcher: professions are compiled-in tables (render, brain, network ids) with no hook.
- See-through drawing from a block entity: transparent facades step aside with a network tool in hand; the anchor's
  overlay is a frame of rails and posts where AE2 has translucent walls.
- Light P2P uses a light block in front of the tunnel.

Jade also has its themes (the jar's seven, and packs'), the overlay's drag-to-place screen, and Home, Mod, Plugins and
Profiles config screens with search, sliders, reset, a live preview and AE2's section. The confirm screen's crafting plan runs
on its own thread and the job starts from that plan; a job asked for another way still plans on the server thread.

Left that could still be done:
- Jade: card layout in the config screens, arrow-key focus, mouse binds, non-English text in them, HSL theme colours.
- AE2: `showPlacementPreview`, `NETWORK_BOOTING`, the nested P2P error, power buffers on charger, inscriber and chest.
- `allowBlockEntities` for facades. AE2's sleeping tick rates (the port has fixed cadences).
- JEI: the panel lists only the bare facade, which belongs to the JEI mod; `craftMissing` has no confirm screen.
- Access point and channel drains: the access point's is charged, the channel drain only displayed.

The dedicated-server engine builds with all five mods (`--server-output`). It hasn't been run: that needs the Minecraft EULA
accepted, which is the owner's to do.
