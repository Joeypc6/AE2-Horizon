ae2_optional::if_mod_items!("jei", {
// AE2's JEI integration (ae2jeiintegration's JEIPlugin), as a layer over the original JEI mod, which it leaves as
// it is. JEI shows vanilla's recipes; this layer hears the same panel events, asks a JEI viewer of its own (JEI's
// public `Viewer`) for vanilla's recipe pages, adds AE2's tabs after them (inscriber, charger, transform, certus
// growth, condenser) and shows the merged page on the next frame, after JEI has drawn its own. With an AE2
// terminal open, every recipe gets the "+" that fills it (AE2's recipe transfer).
use crate::game::Game;
use crate::game::jei::{self, Context, Event, Ingredient, Item, Kind, Layout, Out, Recipe, Stack, Station, View, Viewer};
use crate::game::native_hooks::{NativePanelEvent, NativeRecipeKind, NativeRecipes, NativeRecipeStack, NativeStack};
use crate::game::port_jei_window::{Act, Card, Tab, Transfer};
use horizon_client::hud::rgba;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The Crafting tab's icon.
const CRAFTING: &str = "minecraft:crafting_table";

/// JEIPlugin.registerRecipeCatalysts, AE2's own categories: U on one opens its category whole.
const CATALYSTS: [&str; 5] = ["ae2:condenser", "ae2:inscriber", "ae2:charger", "ae2:crank", "ae2:entropy_manipulator"];

/// One of AE2's recipe tabs: the item on it, how its recipes are laid out, its recipes.
struct Category {
    icon: &'static str,
    title: String,
    catalysts: &'static [&'static str],
    layout: Layout,
    /// The grid's size for `Layout::Grid`; 0 fits each recipe.
    width: u32,
    height: u32,
    recipes: Vec<Ae2Recipe>,
}

struct Ae2Recipe {
    /// Each slot's alternatives (empty: an empty cell).
    slots: Vec<Vec<Stack>>,
    results: Vec<Stack>,
    note: String,
}

/// A recipe page: an item's recipes or uses, on one tab.
#[derive(Clone)]
struct Page {
    item: Stack,
    uses: bool,
    tab: usize,
}

/// What a page shows: its title and tabs. Vanilla's come first, in the order of the layer's JEI viewer.
struct Shown {
    title: String,
    tabs: Vec<Tab>,
    vanilla: usize,
}

struct Layer {
    /// A JEI viewer of the layer's own, asked for vanilla's pages (its history is emptied after each question).
    query: Viewer,
    categories: Vec<Category>,
    /// The panel's list as JEI's search leaves it (JEI's `Clicked` indexes it).
    listed: Vec<Stack>,
    names: std::collections::HashMap<String, String>,
    history: Vec<Page>,
    /// How many of the shown page's tabs are vanilla's.
    vanilla_tabs: usize,
    /// The world's recipes, for a catalyst's whole category.
    catalog: Arc<NativeRecipes>,
    /// The blocks a facade can be of (the content's facade table: FacadeItem.createFacadeForItem's answer).
    facades: Vec<String>,
}

/// The open terminal's list as the server last sent it: amounts by id, and what the grid can craft.
struct Stock {
    amounts: HashMap<String, i64>,
    craftable: HashSet<String>,
}

thread_local! {
    static LAYER: RefCell<Option<Layer>> = const { RefCell::new(None) };
    /// The list a moment ago: every "+" on the page asks each frame, and the list can be long.
    static STOCK: RefCell<Option<(std::time::Instant, Stock)>> = const { RefCell::new(None) };
    /// The panel's list this layer has last seen (`Shown.version`): one comparison a frame until it changes.
    static PANEL_SEEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `ItemModText`: AE2's words for its JEI integration, from the content's language files.
fn text(key: &str, english: &str) -> String {
    horizon_client::lang::get_or(&format!("ae2.rei_jei_integration.{key}"), english)
}

/// `Component.translatable`.
fn tr(key: &str, english: &str) -> String {
    horizon_client::lang::get_or(key, english)
}

/// `native_chat`: JEI's "No recipes for" and "No uses for" go, since the window has more than JEI's own recipes; true
/// to let a line through.
pub fn chat_allowed(text: &str) -> bool {
    !["No recipes for", "No uses for", "Sin recetas para", "Sin usos para"].iter().any(|p| text.starts_with(p))
}

/// The recipe and uses keys, as JEI reads them.
fn keys(game: &Game<'_>) -> (String, String) {
    (game.native_mod_key(jei::ID, "recipe", "key.keyboard.r"), game.native_mod_key(jei::ID, "uses", "key.keyboard.u"))
}

/// The layer, made once the world's tables are there.
fn ensure(game: &mut Game<'_>) -> bool {
    if LAYER.with_borrow(|l| l.is_some()) {
        return true;
    }
    let Some(catalog) = game.native_recipes() else { return false };
    let items: Vec<Item> =
        catalog.items.iter().map(|i| Item { id: i.id.clone(), name: i.name.clone(), tags: i.tags.clone() }).collect();
    let ingredient =
        |s: &crate::game::native_hooks::NativeRecipeStack| Ingredient { item: s.item, count: s.count, components: s.components.clone() };
    let recipes: Vec<Recipe> = catalog
        .recipes
        .iter()
        .filter(|r| !r.slots.is_empty())
        .map(|r| Recipe {
            kind: kind(r.kind),
            width: r.width,
            height: r.height,
            slots: r.slots.iter().map(|s| s.iter().map(ingredient).collect()).collect(),
            results: r.results.iter().map(ingredient).collect(),
            cooking_time: r.cooking_time,
            experience: r.experience,
            book_id: r.book_id,
        })
        .collect();
    let names = items.iter().map(|i| (i.id.clone(), i.name.clone())).collect();
    let lang = jei::lang(game.native_setting(jei::ID, "language").as_deref());
    let (query, out) = Viewer::new(items, recipes, lang);
    let listed = match out {
        Out::Items(stacks) => stacks,
        _ => Vec::new(),
    };
    let categories = categories(game);
    let facades: Vec<String> = game
        .native_data("data/ae2/hz/facades.json")
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|table| table.as_object().map(|o| o.keys().cloned().collect()))
        .unwrap_or_default();
    LAYER.with_borrow_mut(|l| {
        *l = Some(Layer { query, categories, listed, names, history: Vec::new(), vanilla_tabs: 0, catalog, facades });
    });
    true
}

fn kind(kind: NativeRecipeKind) -> Kind {
    match kind {
        NativeRecipeKind::CraftingShaped => Kind::CraftingShaped,
        NativeRecipeKind::CraftingShapeless => Kind::CraftingShapeless,
        NativeRecipeKind::Smelting => Kind::Smelting,
        NativeRecipeKind::Blasting => Kind::Blasting,
        NativeRecipeKind::Smoking => Kind::Smoking,
        NativeRecipeKind::Campfire => Kind::Campfire,
        NativeRecipeKind::Stonecutting => Kind::Stonecutting,
        NativeRecipeKind::Smithing => Kind::Smithing,
        NativeRecipeKind::Brewing => Kind::Brewing,
    }
}

/// `native_panel_event` on JEI's panel: follow the player through recipe pages as JEI does, with AE2's tabs.
pub fn event(game: &mut Game<'_>, mod_id: &str, event: &NativePanelEvent) {
    if mod_id != jei::ID || !game.native_on(jei::ID) || !ensure(game) {
        return;
    }
    let (recipe_key, uses_key) = keys(game);
    let known = game.native_known_recipes();
    let ctx = Context { recipe_key: &recipe_key, uses_key: &uses_key, give: false, creative: false, known: &known };
    let terminal = ae2_terminal_open(game);
    match event {
        NativePanelEvent::Search(text) => LAYER.with_borrow_mut(|l| {
            let Some(l) = l.as_mut() else { return };
            if let Some(Out::Items(stacks)) = l.query.handle(Event::Search(text.clone()), &ctx).into_iter().next() {
                l.listed = stacks;
            }
        }),
        NativePanelEvent::Key { key, stack } => {
            let stack = Stack { item: stack.item.clone(), count: 1, components: stack.components.clone() };
            if *key == recipe_key || *key == uses_key {
                open(game, stack, *key == uses_key, &ctx, terminal);
            } else if key == "key.keyboard.w" {
                // AE2's guide: W over an item, the panel's too.
                crate::game::port_ae2_me::open_guide(game, &stack.item);
            }
        }
        NativePanelEvent::Clicked { index, right } => {
            let stack = LAYER.with_borrow(|l| l.as_ref().and_then(|l| l.listed.get(*index as usize).cloned()));
            // In "click gives items" mode JEI gives it (or opens it in survival); otherwise its recipes. A facade is this
            // layer's, so JEI has nothing to give: its recipe.
            if jei::give(game.native_setting(jei::ID, "give").as_deref()) && !stack.as_ref().is_some_and(|s| is_block_facade(&s.item, &s.components)) {
                return;
            }
            if let Some(stack) = stack {
                open(game, stack, *right, &ctx, terminal);
            }
        }
        _ => {}
    }
}

/// Opens an item's recipes or uses, when vanilla or AE2 has any (JEI says when neither does).
fn open(game: &Game<'_>, item: Stack, uses: bool, ctx: &Context<'_>, terminal: bool) {
    let mut page = Page { item, uses, tab: 0 };
    let shown = page_of(game, &page, ctx);
    if shown.tabs.is_empty() {
        return;
    }
    // A catalyst's uses open on the category it is the catalyst of.
    if uses && is_catalyst(&page.item.item) {
        page.tab = shown.tabs.iter().position(|t| t.catalysts.contains(&page.item.item)).unwrap_or(0);
    }
    LAYER.with_borrow_mut(|l| {
        if let Some(l) = l.as_mut() {
            l.history.push(page.clone());
        }
    });
    show(game, &page, ctx, terminal);
}

/// A vanilla category's title and the blocks it is made on, from its tab's icon.
fn vanilla_tab(icon: &str) -> (&'static str, &'static [&'static str]) {
    match icon {
        // JEIPlugin.registerRecipeCatalysts: the crafting terminals craft too.
        CRAFTING => ("Crafting", &[CRAFTING, "ae2:crafting_terminal", "ae2:wireless_crafting_terminal"]),
        "minecraft:furnace" => ("Smelting", &["minecraft:furnace"]),
        "minecraft:blast_furnace" => ("Blasting", &["minecraft:blast_furnace"]),
        "minecraft:smoker" => ("Smoking", &["minecraft:smoker"]),
        "minecraft:campfire" => ("Campfire Cooking", &["minecraft:campfire", "minecraft:soul_campfire"]),
        "minecraft:stonecutter" => ("Stonecutting", &["minecraft:stonecutter"]),
        "minecraft:smithing_table" => ("Smithing", &["minecraft:smithing_table"]),
        "minecraft:brewing_stand" => ("Brewing", &["minecraft:brewing_stand"]),
        _ => ("Recipes", &[]),
    }
}

/// A catalyst, of AE2's categories or vanilla's.
fn is_catalyst(item: &str) -> bool {
    CATALYSTS.contains(&item) || Station::ALL.iter().any(|s| vanilla_tab(s.icon()).1.contains(&item))
}

/// A crafting recipe as the 3x3 grid has it.
fn full_grid(v: &mut View) {
    let (w, h) = (v.width.clamp(1, 3) as usize, v.height.clamp(1, 3) as usize);
    let mut full = vec![Vec::new(); 9];
    for (i, s) in std::mem::take(&mut v.slots).into_iter().enumerate().take(w * h) {
        full[(i / w) * 3 + i % w] = s;
    }
    v.slots = full;
    (v.width, v.height) = (3, 3);
}

/// A vanilla category's tab.
fn vanilla(icon: &str, mut views: Vec<View>) -> Tab {
    let (title, catalysts) = vanilla_tab(icon);
    if icon == CRAFTING {
        views.iter_mut().for_each(full_grid);
    }
    Tab { icon: icon.to_owned(), title: title.to_owned(), catalysts: catalysts.iter().map(|c| (*c).to_owned()).collect(), cards: views.into_iter().map(Card::Recipe).collect() }
}

/// What tells two of a tab's recipes apart. (A result's components aren't in it: a facade's are written two ways.)
fn view_key(v: &View) -> String {
    let mut key = String::new();
    for slot in &v.slots {
        for s in slot {
            key.push_str(&s.item);
            key.push_str(&s.components);
            key.push(',');
        }
        key.push(';');
    }
    for r in &v.results {
        key.push_str(&r.item);
        key.push(',');
    }
    key
}

/// Adds the recipes a vanilla category's tab hasn't yet, after those it has (JEI's viewer indexes them); the tab
/// itself, after the others, when the page has none.
fn add_to_tab(tabs: &mut Vec<Tab>, icon: &str, mut views: Vec<View>) {
    if views.is_empty() {
        return;
    }
    if icon == CRAFTING {
        views.iter_mut().for_each(full_grid);
    }
    if !tabs.iter().any(|t| t.icon == icon) {
        tabs.push(vanilla(icon, Vec::new()));
    }
    let Some(tab) = tabs.iter_mut().find(|t| t.icon == icon) else { return };
    let mut have: HashSet<String> = tab
        .cards
        .iter()
        .filter_map(|c| match c {
            Card::Recipe(v) => Some(view_key(v)),
            _ => None,
        })
        .collect();
    for v in views {
        if have.insert(view_key(&v)) {
            tab.cards.push(Card::Recipe(v));
        }
    }
}

/// All of a station's recipes, as JEI's viewer lays them out: what U on its catalyst shows.
fn station_views(catalog: &NativeRecipes, station: Station) -> Vec<View> {
    let stack = |s: &NativeRecipeStack| Stack {
        item: catalog.items.get(s.item as usize).map_or_else(|| "minecraft:air".to_owned(), |i| i.id.clone()),
        count: s.count,
        components: s.components.clone(),
    };
    catalog
        .recipes
        .iter()
        .filter(|r| !r.slots.is_empty() && kind(r.kind).station() == station)
        .map(|r| {
            let n = r.slots.len() as u32;
            let (layout, width, height, note) = match kind(r.kind) {
                Kind::CraftingShaped => (Layout::Grid, r.width, r.height, String::new()),
                Kind::CraftingShapeless => {
                    let w = match n {
                        0 | 1 => 1,
                        2..=4 => 2,
                        _ => 3,
                    };
                    (Layout::Grid, w, n.div_ceil(w).max(1), "Shapeless".to_owned())
                }
                Kind::Smelting | Kind::Blasting | Kind::Smoking | Kind::Campfire => {
                    (Layout::Cooking, 1, 1, format!("{} s, {} XP", r.cooking_time as f32 / 20.0, (r.experience * 100.0).round() / 100.0))
                }
                Kind::Stonecutting => (Layout::Single, 1, 1, String::new()),
                Kind::Smithing => (Layout::Smithing, 3, 1, String::new()),
                Kind::Brewing => (Layout::Brewing, 2, 1, String::new()),
            };
            View {
                layout,
                width,
                height,
                slots: r.slots.iter().map(|s| s.iter().take(jei::STACKS_MAX).map(&stack).collect()).collect(),
                results: r.results.iter().take(jei::STACKS_MAX).map(&stack).collect(),
                note,
                action: None,
            }
        })
        .collect()
}

/// FacadeRegistryPlugin.make: four cable anchors around the block make four facades of it.
fn facade_view(block: &str) -> View {
    let mut slots: Vec<Vec<Stack>> = vec![Vec::new(); 9];
    for i in [1, 3, 5, 7] {
        slots[i] = one("ae2:cable_anchor");
    }
    slots[4] = one(block);
    View { layout: Layout::Grid, width: 3, height: 3, slots, results: vec![facade_stack(block, 4)], note: String::new(), action: None }
}

/// A facade of `block`, as FacadeItem.createFacadeForItem makes it.
fn facade_stack(block: &str, count: u32) -> Stack {
    let (ns, path) = block.split_once(':').unwrap_or(("minecraft", block));
    let flat = if ns == "minecraft" { path.to_owned() } else { format!("{ns}_{path}") };
    let components = format!("[minecraft:custom_data={{item:\"{block}\"}},minecraft:item_model=\"ae2:hz_facade_{flat}\"]");
    Stack { item: "ae2:facade".to_owned(), count, components }
}

/// A facade of some block, not the one of none that JEI lists.
fn is_block_facade(item: &str, components: &str) -> bool {
    item == "ae2:facade" && !components.is_empty()
}

/// The facades a search leaves: every word in the facade's name, its block's or its block's id (`@ae2` for the mod).
fn facades_for(l: &Layer, search: &str) -> Vec<Stack> {
    let words: Vec<String> = search.to_lowercase().split_whitespace().map(str::to_owned).collect();
    let facade = l.names.get("ae2:facade").map_or("facade", String::as_str).to_lowercase();
    l.facades
        .iter()
        .filter(|block| {
            let name = l.names.get(*block).map_or("", String::as_str).to_lowercase();
            words.iter().all(|w| match w.strip_prefix('@') {
                Some(m) => "ae2".contains(m),
                None => facade.contains(w.as_str()) || name.contains(w.as_str()) || block.contains(w.as_str()),
            })
        })
        .map(|block| facade_stack(block, 1))
        .collect()
}

/// JEIPlugin, unless `enableFacadesInJEI` is off: the facades in the ingredient list, as AE2's creative tab has them. JEI's viewer lists
/// each item once, so this layer puts them after its list whenever JEI has sent one, and clicks past JEI's end are this
/// layer's alone (`listed`).
fn list_facades(game: &mut Game<'_>) {
    use crate::game::native_panel::port_jei_panel_size as panel;
    let Some((version, search, mut items)) = panel::changed_items(game, jei::ID, PANEL_SEEN.get()) else { return };
    PANEL_SEEN.set(version);
    if !crate::game::port_ae2_me::facades_in_jei() || items.iter().any(|s| is_block_facade(&s.item, &s.components)) || !game.native_on(jei::ID) || !ensure(game) {
        return;
    }
    let added = LAYER.with_borrow(|l| l.as_ref().map_or_else(Vec::new, |l| facades_for(l, &search)));
    if added.is_empty() {
        return;
    }
    // JEI's empty list keeps one invisible slot, which isn't wanted now.
    items.retain(|s| s.item != "minecraft:air");
    items.extend(added.into_iter().map(|s| NativeStack { item: s.item, count: s.count, components: s.components }));
    LAYER.with_borrow_mut(|l| {
        if let Some(l) = l.as_mut() {
            l.listed = items.iter().map(|s| Stack { item: s.item.clone(), count: s.count, components: s.components.clone() }).collect();
        }
    });
    game.native_panel_items(jei::ID, items);
    PANEL_SEEN.set(panel::items_version(game));
}

/// FacadeRegistryPlugin.getRecipes: a block's uses have its facade, a facade's recipes the recipe of that facade (JEI's
/// viewer stops at 256 of a tab's recipes; the panel's facade, of no block, stands for them all).
fn facade_views(facades: &[String], page: &Page) -> Vec<View> {
    if page.uses {
        return facades.iter().filter(|b| **b == page.item.item).map(|b| facade_view(b)).collect();
    }
    if page.item.item != "ae2:facade" {
        return Vec::new();
    }
    // FacadeItem.getTextureItem: the block's id, quoted, in the stack's components.
    let of_this: Vec<View> = facades.iter().filter(|b| page.item.components.contains(&format!("\"{b}\""))).map(|b| facade_view(b)).collect();
    if of_this.is_empty() { facades.iter().map(|b| facade_view(b)).collect() } else { of_this }
}

/// Vanilla's tabs for a page (from the layer's own JEI viewer), then AE2's, then where it comes from.
fn page_of(game: &Game<'_>, page: &Page, ctx: &Context<'_>) -> Shown {
    LAYER.with_borrow_mut(|l| {
        let Some(l) = l.as_mut() else { return Shown { title: String::new(), tabs: Vec::new(), vanilla: 0 } };
        let key = if page.uses { ctx.uses_key } else { ctx.recipe_key };
        let mut tabs: Vec<Tab> = Vec::new();
        let first = l.query.handle(Event::Key { key: key.to_owned(), stack: page.item.clone() }, ctx);
        if let Some(Out::Recipes { tabs: icons, views, .. }) = first.into_iter().next()
            && !icons.is_empty()
        {
            tabs.push(vanilla(&icons[0].item, views));
            for (t, icon) in icons.iter().enumerate().skip(1) {
                if let Some(Out::Recipes { views, .. }) = l.query.handle(Event::Tab(t), ctx).into_iter().next() {
                    tabs.push(vanilla(&icon.item, views));
                }
            }
            // Back to an empty history for the next question.
            l.query.handle(Event::Back, ctx);
        }
        let vanilla_tabs = tabs.len();
        // JEI's catalysts: U on a station, or on a crafting terminal, has its whole category.
        if page.uses {
            for station in Station::ALL {
                if vanilla_tab(station.icon()).1.contains(&page.item.item.as_str()) {
                    add_to_tab(&mut tabs, station.icon(), station_views(&l.catalog, station));
                }
            }
        }
        add_to_tab(&mut tabs, CRAFTING, facade_views(&l.facades, page));
        for c in &l.categories {
            let wanted = |s: &Stack| s.item == page.item.item;
            // U on the category's catalyst: all of it.
            let whole = page.uses && CATALYSTS.contains(&page.item.item.as_str()) && c.catalysts.contains(&page.item.item.as_str());
            let cards: Vec<Card> = c
                .recipes
                .iter()
                .filter(|r| whole || if page.uses { r.slots.iter().flatten().any(wanted) } else { r.results.iter().any(wanted) })
                .map(|r| Card::Recipe(ae2_view(game, c, r)))
                .collect();
            if !cards.is_empty() {
                tabs.push(Tab { icon: c.icon.to_owned(), title: c.title.clone(), catalysts: c.catalysts.iter().map(|s| (*s).to_owned()).collect(), cards });
            }
        }
        if !page.uses
            && let Some(text) = crate::game::port_ae2_guide::summary(game, &page.item.item)
        {
            let lines = crate::game::port_jei_collection::wrap_text(game, &text);
            tabs.push(Tab { icon: "minecraft:book".to_owned(), title: "Information".to_owned(), catalysts: Vec::new(), cards: vec![Card::Text(lines)] });
        }
        // Where it comes from when not crafted, or what a spawn egg's mob drops and who trades for it.
        tabs.extend(if page.uses { crate::game::port_jei_collection::use_tabs(game, &page.item.item) } else { crate::game::port_jei_collection::got_tabs(game, &page.item.item) });
        let name = l.names.get(&page.item.item).cloned().unwrap_or_else(|| page.item.item.clone());
        Shown { title: format!("{}: {name}", if page.uses { "Uses" } else { "Recipes" }), tabs, vanilla: vanilla_tabs }
    })
}

fn ae2_view(game: &Game<'_>, c: &Category, r: &Ae2Recipe) -> View {
    let (mut layout, mut width, mut height) = (c.layout, c.width, c.height);
    if layout == Layout::Grid && width == 0 {
        let n = r.slots.len() as u32;
        width = n.clamp(1, 3);
        height = n.div_ceil(width).max(1);
    }
    // A page of a one-slot category with more slots (CertusGrowthCategory.GETTING_BUDDING_QUARTZ): side by side.
    if layout == Layout::Single && r.slots.len() > 1 {
        layout = Layout::Grid;
        width = r.slots.len() as u32;
        height = 1;
    }
    // The note as the card's width has it.
    let note = r.note.split('\n').flat_map(|line| crate::game::port_jei_collection::wrap_text(game, line)).collect::<Vec<_>>().join("\n");
    View { layout, width, height, slots: r.slots.clone(), results: r.results.clone(), note, action: None }
}

/// The merged page, in the window. On the pattern encoding terminal every recipe (not a source) gets the "+" that
/// encodes it (EncodePatternTransferHandler is universal); on a crafting terminal only the Crafting tab's do
/// (UseCraftingRecipeTransfer takes `RecipeTypes.CRAFTING` and no other: a machine's recipe has none).
fn show(game: &Game<'_>, page: &Page, ctx: &Context<'_>, terminal: bool) {
    let mut shown = page_of(game, page, ctx);
    if shown.tabs.is_empty() {
        return;
    }
    let tab = page.tab.min(shown.tabs.len() - 1);
    if terminal {
        let encoding = encoding_open(game);
        for t in &mut shown.tabs {
            let crafting = t.icon == CRAFTING;
            for c in &mut t.cards {
                if let Card::Recipe(v) = c {
                    let takes = if encoding { !v.results.is_empty() } else { crafting && v.layout == Layout::Grid };
                    v.action = takes.then_some(true);
                }
            }
        }
    }
    LAYER.with_borrow_mut(|l| {
        if let Some(l) = l.as_mut() {
            l.vanilla_tabs = shown.vanilla;
        }
    });
    crate::game::port_jei_window::show(shown.title, shown.tabs, tab);
}

/// Once a frame: what the player did on the window, and the panel keeps its items.
pub fn frame(game: &mut Game<'_>) {
    if game.native_player().is_none() {
        LAYER.with_borrow_mut(|l| *l = None);
        crate::game::port_jei_window::close();
        return;
    }
    crate::game::port_jei_collection::frame(game);
    crate::game::port_jei_creative::frame(game);
    if crate::game::native_panel::port_jei_panel_size::has_recipes(game) {
        game.native_panel_recipes(jei::ID, "", Vec::new(), 0, Vec::new());
    }
    list_facades(game);
    // The window goes with its screen.
    if matches!(game.screen, crate::game::Screen::Playing) && crate::game::port_jei_window::is_open() {
        crate::game::port_jei_window::close();
        LAYER.with_borrow_mut(|l| {
            if let Some(l) = l.as_mut() {
                l.history.clear();
            }
        });
    }
    crate::game::port_jei_window::frame(game);
    let acts = crate::game::port_jei_window::take_acts();
    if acts.is_empty() {
        return;
    }
    let (recipe_key, uses_key) = keys(game);
    let known = game.native_known_recipes();
    let ctx = Context { recipe_key: &recipe_key, uses_key: &uses_key, give: false, creative: false, known: &known };
    let terminal = ae2_terminal_open(game);
    for act in acts {
        match act {
            Act::Tab(tab) => go_to_tab(game, &ctx, terminal, |_| tab),
            Act::NextTab(by) => {
                let n = crate::game::port_jei_window::tab_count().max(1) as i32;
                go_to_tab(game, &ctx, terminal, |now| (now as i32 + by).rem_euclid(n) as usize);
            }
            Act::Page(by) => crate::game::port_jei_window::turn_page(by, crate::game::port_jei_window::cards_area(game)),
            Act::Slot { stack, uses } => open(game, Stack { count: 1, ..stack }, uses, &ctx, terminal),
            Act::Key { key, stack } => {
                let stack = Stack { count: 1, ..stack };
                if key == recipe_key || key == uses_key {
                    open(game, stack, key == uses_key, &ctx, terminal);
                } else if key == "key.keyboard.w" {
                    crate::game::port_ae2_me::open_guide(game, &stack.item);
                }
            }
            Act::Close => {
                crate::game::port_jei_window::close();
                LAYER.with_borrow_mut(|l| {
                    if let Some(l) = l.as_mut() {
                        l.history.clear();
                    }
                });
            }
            Act::Back => {
                let page = LAYER.with_borrow_mut(|l| {
                    let l = l.as_mut()?;
                    l.history.pop();
                    l.history.last().cloned()
                });
                match page {
                    Some(page) => show(game, &page, &ctx, terminal),
                    None => crate::game::port_jei_window::close(),
                }
            }
            Act::Plus(index) => plus(game, index, &ctx, terminal),
            // JEIPlugin.getGuiClickableAreas: the category, as U on its catalyst has it.
            Act::Category(item) => {
                if game.native_on(jei::ID) && ensure(game) {
                    open(game, Stack::item(&item), true, &ctx, terminal);
                }
            }
        }
    }
}

fn go_to_tab(game: &Game<'_>, ctx: &Context<'_>, terminal: bool, tab: impl Fn(usize) -> usize) {
    let page = LAYER.with_borrow_mut(|l| {
        let top = l.as_mut()?.history.last_mut()?;
        top.tab = tab(top.tab);
        Some(top.clone())
    });
    if let Some(page) = page {
        show(game, &page, ctx, terminal);
    }
}

/// The "+": on an AE2 terminal the recipe fills it; elsewhere the game's recipe book places it.
fn plus(game: &mut Game<'_>, index: usize, ctx: &Context<'_>, terminal: bool) {
    let Some(page) = LAYER.with_borrow(|l| l.as_ref().and_then(|l| l.history.last().cloned())) else { return };
    let vanilla = LAYER.with_borrow(|l| l.as_ref().map_or(0, |l| l.vanilla_tabs));
    if terminal {
        if let Some(view) = crate::game::port_jei_window::recipe_view(index) {
            // What the last frame drew may have gone since.
            if transfer_state(game, &view).is_some_and(|s| !s.enabled) {
                return;
            }
            let crafting = crate::game::port_jei_window::shown_icon().as_deref() == Some(CRAFTING) && view.layout == Layout::Grid;
            // UseCraftingRecipeTransfer: craftMissing is Control held (the encoder has nothing to craft).
            let craft_missing = crafting && !encoding_open(game) && game.has_control_down();
            let wanted = if craft_missing { to_craft(game, &view) } else { Vec::new() };
            transfer(game, &view, crafting, false);
            if !wanted.is_empty() {
                // JEI goes back to the screen, where the confirm dialog opens.
                crate::game::port_jei_window::close();
                LAYER.with_borrow_mut(|l| {
                    if let Some(l) = l.as_mut() {
                        l.history.clear();
                    }
                });
                crate::game::port_ae2_me::jei_craft_missing(wanted);
            }
        }
        return;
    }
    if page.tab >= vanilla {
        return;
    }
    let key = if page.uses { ctx.uses_key } else { ctx.recipe_key };
    let out = LAYER.with_borrow_mut(|l| {
        let l = l.as_mut()?;
        l.query.handle(Event::Key { key: key.to_owned(), stack: page.item.clone() }, ctx);
        l.query.handle(Event::Tab(page.tab), ctx);
        let out = l.query.handle(Event::Action(index), ctx);
        l.query.handle(Event::Back, ctx);
        Some(out)
    });
    for o in out.into_iter().flatten() {
        if let Out::Place { book_id, all } = o {
            let _ = game.native_place_recipe(book_id, all);
        }
    }
}

/// An AE2 terminal that takes recipes is open (`AEGuiContainerHandler`).
fn ae2_terminal_open(game: &Game<'_>) -> bool {
    matches!(game.native_screen_title().as_deref(), Some("ME Pattern Encoding Terminal" | "ME Crafting Terminal")) || crate::game::port_ae2_me::wireless_crafting_open()
}

/// The pattern encoding terminal is open (the crafting terminals fill their grid instead).
fn encoding_open(game: &Game<'_>) -> bool {
    game.native_screen_title().as_deref() == Some("ME Pattern Encoding Terminal")
}

/// The terminal's list, read again four times a second.
fn with_stock<R>(f: impl FnOnce(&Stock) -> R) -> R {
    STOCK.with_borrow_mut(|cached| {
        if cached.as_ref().is_none_or(|(at, _)| at.elapsed().as_millis() >= 250) {
            let (amounts, craftable) = crate::game::port_ae2_me::jei_terminal_stock();
            *cached = Some((std::time::Instant::now(), Stock { amounts, craftable }));
        }
        match cached.as_ref() {
            Some((_, stock)) => f(stock),
            None => f(&Stock { amounts: HashMap::new(), craftable: HashSet::new() }),
        }
    })
}

/// TransferHelper's colours (the ints AE2 gives them, as this window's).
fn blue_text() -> u32 {
    rgba(85, 85, 255, 255)
}

fn red_text() -> u32 {
    rgba(255, 85, 85, 255)
}

fn blue_slot() -> u32 {
    rgba(0, 0, 255, 64)
}

fn red_slot() -> u32 {
    rgba(255, 0, 0, 102)
}

/// What a recipe's "+" says on the open AE2 terminal; none on any other screen.
pub fn transfer_state(game: &Game<'_>, view: &View) -> Option<Transfer> {
    if !ae2_terminal_open(game) {
        return None;
    }
    Some(if encoding_open(game) { encode_state(view) } else { crafting_state(game, view) })
}

/// EncodePatternTransferHandler.findCraftableSlots and its ErrorRenderer: the slots the network can already craft
/// marked, and the tooltip of TransferHelper.createEncodingTooltip.
fn encode_state(view: &View) -> Transfer {
    let craftable: Vec<usize> =
        with_stock(|stock| view.slots.iter().enumerate().filter(|(_, slot)| slot.iter().any(|s| stock.craftable.contains(&s.item))).map(|(i, _)| i).collect());
    let mut lines = vec![(text("encode_pattern", "Encode pattern"), u32::MAX)];
    if !craftable.is_empty() {
        lines.push((text("has_encoded_ingredients", "Highlighted elements are already craftable"), blue_text()));
    }
    Transfer { enabled: true, tint: None, lines, marks: craftable.into_iter().map(|i| (i, blue_slot())).collect() }
}

/// UseCraftingRecipeTransfer.transferRecipe, with CraftingTermMenu.findMissingIngredients: each slot looks in the
/// player's inventory, then the network, then what the network can craft, each counted so that two slots don't both
/// take the one there is.
fn crafting_state(game: &Game<'_>, view: &View) -> Transfer {
    let refused = |why: String, marks: Vec<(usize, u32)>| Transfer {
        enabled: false,
        tint: None,
        lines: vec![(text("move_items", "Move items"), u32::MAX), (why, red_text())],
        marks,
    };
    let wanted: Vec<usize> = (0..view.slots.len()).filter(|i| !view.slots[*i].is_empty()).collect();
    if wanted.is_empty() {
        return refused(text("incompatible_recipe", "Incompatible recipe"), Vec::new());
    }
    let (missing, craftable) = find_ingredients(game, view, &wanted);
    // Nothing at all to move: the "+" is off, and the slots say why.
    if missing.len() == wanted.len() {
        return refused(text("no_items", "Found no compatible items"), missing.into_iter().map(|i| (i, red_slot())).collect());
    }
    // TransferHelper.createCraftingTooltip.
    let mut lines = vec![(text("move_items", "Move items"), u32::MAX)];
    if !craftable.is_empty() {
        let line = if game.has_control_down() { text("will_craft", "Will craft unavailable items") } else { text("ctrl_click_to_craft", "CTRL + click to craft unavailable items") };
        lines.push((line, blue_text()));
    }
    if !missing.is_empty() {
        lines.push((text("missing_items", "Missing items will be skipped"), red_text()));
    }
    // TransferHelper.ORANGE_PLUS_BUTTON_COLOR and BLUE_PLUS_BUTTON_COLOR.
    let tint = if !missing.is_empty() {
        Some(rgba(255, 165, 0, 128))
    } else if !craftable.is_empty() {
        Some(rgba(69, 69, 255, 128))
    } else {
        None
    };
    let marks = missing.into_iter().map(|i| (i, red_slot())).chain(craftable.into_iter().map(|i| (i, blue_slot()))).collect();
    Transfer { enabled: true, tint, lines, marks }
}

/// CraftingTermMenu.findMissingIngredients: of the slots `wanted`, those found nowhere and those only the network can
/// craft.
fn find_ingredients(game: &Game<'_>, view: &View, wanted: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let mut inventory: HashMap<&str, u32> = HashMap::new();
    for s in game.inventory.iter().take(36).flatten() {
        *inventory.entry(&*s.item).or_default() += s.count;
    }
    let (mut missing, mut craftable) = (Vec::new(), Vec::new());
    with_stock(|stock| {
        let mut reserved: HashMap<&str, i64> = HashMap::new();
        for &i in wanted {
            let slot = &view.slots[i];
            if let Some(s) = slot.iter().find(|s| inventory.get(s.item.as_str()).is_some_and(|n| *n > 0)) {
                if let Some(n) = inventory.get_mut(s.item.as_str()) {
                    *n -= 1;
                }
                continue;
            }
            if let Some(s) = slot.iter().find(|s| stock.amounts.get(&s.item).copied().unwrap_or(0) > reserved.get(s.item.as_str()).copied().unwrap_or(0)) {
                *reserved.entry(s.item.as_str()).or_default() += 1;
                continue;
            }
            if slot.iter().any(|s| stock.craftable.contains(&s.item)) {
                craftable.push(i);
            } else {
                missing.push(i);
            }
        }
    });
    (missing, craftable)
}

/// FillCraftingGridFromRecipePacket: what the "+" finds nowhere that the network can craft, a key and the slots wanting it
/// (AutoCraftEntry).
fn to_craft(game: &Game<'_>, view: &View) -> Vec<(String, i64)> {
    let wanted: Vec<usize> = (0..view.slots.len()).filter(|i| !view.slots[*i].is_empty()).collect();
    let (_, craftable) = find_ingredients(game, view, &wanted);
    let mut list: Vec<(String, i64)> = Vec::new();
    with_stock(|stock| {
        for i in craftable {
            let Some(s) = view.slots[i].iter().find(|s| stock.craftable.contains(&s.item)) else { continue };
            match list.iter_mut().find(|(id, _)| *id == s.item) {
                Some((_, n)) => *n += 1,
                None => list.push((s.item.clone(), 1)),
            }
        }
    });
    list
}

/// The "+" on an AE2 terminal: the recipe to the server (`ae2:transfer`), which fills the open terminal: `mode`,
/// `size`, then each slot's alternatives and amount (`s index id,id amount`) and the results (`r id amount`). `craft`
/// asks it to start crafting what it finds nowhere (FillCraftingGridFromRecipePacket's craftMissing).
fn transfer(game: &mut Game<'_>, view: &View, crafting: bool, craft_missing: bool) {
    let mut text = format!("mode {}\nsize {} {}\n", if crafting { "crafting" } else { "processing" }, view.width.max(1), view.height.max(1));
    if craft_missing {
        text.push_str("craft\n");
    }
    for (i, slot) in view.slots.iter().enumerate() {
        if let Some(first) = slot.first() {
            let ids: Vec<&str> = slot.iter().map(|s| s.item.as_str()).collect();
            text.push_str(&format!("s {i} {} {}\n", ids.join(","), first.count.max(1)));
        }
    }
    for r in &view.results {
        text.push_str(&format!("r {} {}\n", r.item, r.count.max(1)));
    }
    if let Err(e) = game.native_net_send_to_server(crate::game::port_ae2_me::NET_TRANSFER, text.as_bytes()) {
        log::info!("ae2: recipe transfer not sent: {e}");
    }
}

// ---------------------------------------------------------------------------------------------------------
// AE2's categories (ae2jeiintegration's categories package).

fn categories(game: &Game<'_>) -> Vec<Category> {
    vec![inscriber(game), charger(game), transform(game), certus_growth(), condenser(), entropy(game), attunement(game), fluids(game)]
}

/// An ingredient as a recipe's JSON names it: each item it takes (a tag's items).
fn alternatives(game: &Game<'_>, json: Option<&serde_json::Value>) -> Vec<Stack> {
    json.and_then(|v| game.native_recipe_ingredient(v)).map_or_else(Vec::new, |i| i.order.iter().map(|id| Stack::item(id)).collect())
}

fn result(game: &Game<'_>, json: &serde_json::Value) -> Vec<Stack> {
    json.get("result")
        .and_then(|r| game.native_recipe_result(r))
        .map_or_else(Vec::new, |s| vec![Stack { item: s.item.to_string(), count: s.count as u32, components: String::new() }])
}

fn one(id: &str) -> Vec<Stack> {
    vec![Stack::item(id)]
}

/// `InscriberRecipeCategory`: top press, middle and bottom press over each other, then the result.
fn inscriber(game: &Game<'_>) -> Category {
    let recipes = game
        .native_recipes_of_type("ae2:inscriber")
        .iter()
        .map(|r| {
            let json = &*r.json;
            let ing = |slot: &str| alternatives(game, json.get("ingredients").and_then(|i| i.get(slot)));
            let press = json.get("mode").and_then(|m| m.as_str()) == Some("press");
            Ae2Recipe {
                slots: vec![ing("top"), ing("middle"), ing("bottom")],
                results: result(game, json),
                note: if press { "Press: uses the presses up" } else { "Inscribe: the presses stay" }.to_owned(),
            }
        })
        .collect();
    Category { icon: "ae2:inscriber", title: tr("block.ae2.inscriber", "Inscriber"), catalysts: &["ae2:inscriber"], layout: Layout::Grid, width: 1, height: 3, recipes }
}

/// `ChargerCategory`: the item on the charger's plate and what it becomes.
fn charger(game: &Game<'_>) -> Category {
    let recipes = game
        .native_recipes_of_type("ae2:charger")
        .iter()
        .map(|r| {
            let json = &*r.json;
            Ae2Recipe {
                slots: vec![alternatives(game, json.get("ingredient"))],
                results: result(game, json),
                note: text("charger_required_power", "%d turns or %d AE").replacen("%d", "10", 1).replacen("%d", "1600", 1),
            }
        })
        .collect();
    Category { icon: "ae2:charger", title: tr("block.ae2.charger", "Charger"), catalysts: &["ae2:charger", "ae2:crank"], layout: Layout::Single, width: 1, height: 1, recipes }
}

/// `TransformCategory`: items thrown together into water (or caught in an explosion) become the result.
fn transform(game: &Game<'_>) -> Category {
    let recipes = game
        .native_recipes_of_type("ae2:transform")
        .iter()
        .map(|r| {
            let json = &*r.json;
            let slots = json
                .get("ingredients")
                .and_then(|i| i.as_array())
                .map_or_else(Vec::new, |list| list.iter().map(|v| alternatives(game, Some(v))).collect());
            let circumstance = json.get("circumstance").and_then(|c| c.get("type")).and_then(|t| t.as_str());
            let note = if circumstance == Some("explosion") {
                text("explosion", "Explosion")
            } else {
                format!("{} {}", text("submerge_in", "Submerge in"), tr("block.minecraft.water", "Water"))
            };
            Ae2Recipe { slots, results: result(game, json), note }
        })
        .collect();
    Category { icon: "minecraft:water_bucket", title: text("transform_category", "In-World Transformation"), catalysts: &["minecraft:water_bucket"], layout: Layout::Grid, width: 0, height: 0, recipes }
}

/// `CertusGrowthCategory`: budding quartz grows buds, buds grow into clusters, clusters break into crystals;
/// imperfect budding quartz decays as it grows.
fn certus_growth() -> Category {
    let step = |from: &str, to: &str, note: &str| Ae2Recipe { slots: vec![one(from)], results: one(to), note: note.to_owned() };
    let mut recipes = Vec::new();
    let grows_buds = text("quartz_buds_grow_on_budding_quartz", "Quartz buds grow on budding quartz.");
    recipes.push(step("ae2:flawless_budding_quartz", "ae2:small_quartz_bud", &format!("{grows_buds}\n{}", text("flawless_budding_quartz_description", "Flawless budding quartz never decays when growing buds."))));
    for budding in ["flawed", "chipped", "damaged"] {
        recipes.push(step(&format!("ae2:{budding}_budding_quartz"), "ae2:small_quartz_bud", &grows_buds));
    }
    let accelerated = text("crystal_growth_accelerators_effect", "Powered crystal growth accelerators speed up the growth of adjacent budding quartz.");
    recipes.push(step("ae2:small_quartz_bud", "ae2:medium_quartz_bud", &accelerated));
    recipes.push(step("ae2:medium_quartz_bud", "ae2:large_quartz_bud", &accelerated));
    recipes.push(step("ae2:large_quartz_bud", "ae2:quartz_cluster", &accelerated));
    recipes.push(Ae2Recipe {
        slots: vec![one("ae2:quartz_cluster")],
        results: vec![Stack { item: "ae2:certus_quartz_crystal".into(), count: 4, components: String::new() }],
        note: format!("{}\n{}", text("fully_grown_buds_drop_crystals", "A fully grown quartz bud drops crystals."), text("fortune_applies", "Fortune enchantment applies")),
    });
    let decays = text("imperfect_budding_quartz_decays", "Imperfect budding quartz has a chance to decay when buds grow.");
    recipes.push(step("ae2:flawed_budding_quartz", "ae2:chipped_budding_quartz", &decays));
    recipes.push(step("ae2:chipped_budding_quartz", "ae2:damaged_budding_quartz", &decays));
    recipes.push(step("ae2:damaged_budding_quartz", "ae2:quartz_block", &decays));
    let stacks = |ids: &[&str]| -> Vec<Stack> { ids.iter().map(|id| Stack::item(id)).collect() };
    // BUDDING_QUARTZ_VARIANTS and BUDDING_QUARTZ_DECAY_ORDER: a slot's stacks turn in step with the other's.
    let variants = stacks(&["ae2:damaged_budding_quartz", "ae2:chipped_budding_quartz", "ae2:flawed_budding_quartz", "ae2:flawless_budding_quartz"]);
    let decay = stacks(&["ae2:quartz_block", "ae2:damaged_budding_quartz", "ae2:chipped_budding_quartz", "ae2:flawed_budding_quartz"]);
    // Page.BUD_LOOT.
    recipes.push(Ae2Recipe {
        slots: vec![stacks(&["ae2:small_quartz_bud", "ae2:medium_quartz_bud", "ae2:large_quartz_bud"])],
        results: one("ae2:certus_quartz_dust"),
        note: text("buds_drop_dust_when_not_fully_grown", "A quartz bud drops dust when not fully grown."),
    });
    // Page.BUDDING_QUARTZ_MOVING.
    recipes.push(Ae2Recipe {
        slots: vec![variants],
        results: decay.clone(),
        note: format!(
            "{}\n{}\n{}",
            text("budding_quartz_decays_when_broken", "Budding quartz decays when broken."),
            text("silk_touch_prevents_decay_for_imperfect", "Silk touch prevents decay for imperfect budding quartz."),
            text("spatial_io_never_causes_any_decay", "Spatial I/O never causes any decay.")
        ),
    });
    // Page.GETTING_BUDDING_QUARTZ.
    recipes.push(Ae2Recipe {
        slots: vec![decay, one("ae2:charged_certus_quartz_crystal"), one("ae2:meteorite_compass")],
        results: Vec::new(),
        note: text("budding_quartz_creation_and_worldgen", "Budding quartz can be found in meteorites, or be regenerated using charged quartz in water."),
    });
    Category { icon: "ae2:flawless_budding_quartz", title: text("certus_quartz_growth", "Certus Quartz Growth"), catalysts: &["ae2:growth_accelerator"], layout: Layout::Single, width: 1, height: 1, recipes }
}

/// `EntropyManipulatorCategory`: a block (or fluid) heated or cooled, and what it becomes or drops.
fn entropy(game: &Game<'_>) -> Category {
    // Water and lava as the fluids themselves (their animated texture), as AE2's JEI integration shows them.
    let as_stacks = |id: &str| match id {
        "minecraft:water" | "minecraft:flowing_water" => fluid("water", "Water"),
        "minecraft:lava" | "minecraft:flowing_lava" => fluid("lava", "Lava"),
        i => one(i),
    };
    let recipes = game
        .native_recipes_of_type("ae2:entropy")
        .iter()
        .filter_map(|r| {
            let j = &*r.json;
            let input = j.get("input")?;
            let id = input.get("block").or_else(|| input.get("fluid"))?.get("id")?.as_str()?;
            let out = j.get("output");
            let mut results: Vec<Stack> = Vec::new();
            if let Some(b) = out.and_then(|o| o.get("block")).and_then(|b| b.get("id")).and_then(|v| v.as_str())
                && b != "minecraft:air"
            {
                results.extend(as_stacks(b));
            }
            for d in out.and_then(|o| o.get("drops")).and_then(|d| d.as_array()).into_iter().flatten() {
                if let Some(i) = d.get("item").and_then(|i| i.as_str()) {
                    results.push(Stack::item(i));
                }
            }
            let mode = j.get("mode").and_then(|m| m.as_str()).unwrap_or("heat");
            // ENTROPY_MANIPULATOR_HEAT and _COOL, over RIGHT_CLICK and SHIFT_RIGHT_CLICK.
            let note = if mode == "cool" {
                format!("{}\n{}", text("entropy_manipulator_cool", "Cool (%d AE)").replace("%d", "1600"), text("shift_right_click", "Shift+Right-Click"))
            } else {
                format!("{}\n{}", text("entropy_manipulator_heat", "Heat (%d AE)").replace("%d", "1600"), text("right_click", "Right-Click"))
            };
            Some(Ae2Recipe { slots: vec![as_stacks(id)], results, note })
        })
        .collect();
    Category { icon: "ae2:entropy_manipulator", title: tr("item.ae2.entropy_manipulator", "Entropy Manipulator"), catalysts: &["ae2:entropy_manipulator"], layout: Layout::Single, width: 1, height: 1, recipes }
}

/// `AttunementCategory`, as JEIPlugin.registerP2PAttunement fills it: the kinds with an item API first (InitP2PAttunements:
/// every item that has it), then what each kind's `ae2:p2p_attunements/*` tag holds. A list with nothing in it goes.
fn attunement(game: &Game<'_>) -> Category {
    let mut recipes = Vec::new();
    for (kind, description) in [("fe_p2p_tunnel", tr("gui.ae2.P2PAttunementEnergy", "Portable Energy Storage (i.e. Batteries)")), ("fluid_p2p_tunnel", tr("gui.ae2.P2PAttunementFluid", "Portable Fluid Storage (i.e. Tanks, Buckets)"))] {
        let items: Vec<Stack> =
            game.res.items().registry_order.iter().map(|id| &**id).filter(|id| crate::game::port_ae2_me::attunement_api(id) == Some(kind)).map(Stack::item).collect();
        if !items.is_empty() {
            recipes.push(Ae2Recipe { slots: vec![items], results: one(&format!("ae2:{kind}")), note: format!("{} {description}", text("p2p_api_attunement", "Attune with any:")) });
        }
    }
    for kind in ["me_p2p_tunnel", "item_p2p_tunnel", "fluid_p2p_tunnel", "redstone_p2p_tunnel", "light_p2p_tunnel", "fe_p2p_tunnel"] {
        let items = alternatives(game, Some(&serde_json::Value::String(format!("#ae2:p2p_attunements/{kind}"))));
        if !items.is_empty() {
            recipes.push(Ae2Recipe { slots: vec![items], results: one(&format!("ae2:{kind}")), note: text("p2p_tag_attunement", "Attune with any shown item") });
        }
    }
    Category { icon: "ae2:me_p2p_tunnel", title: text("p2p_tunnel_attunement", "P2P Tunnel Attunement"), catalysts: &["ae2:me_p2p_tunnel"], layout: Layout::Single, width: 1, height: 1, recipes }
}

/// `CondenserCategory`: anything put in becomes matter balls or singularities, the component in it holding the power.
fn condenser() -> Category {
    let recipes = vec![
        // CondenserCategory.getTooltipStrings.
        Ae2Recipe { slots: vec![one("ae2:cell_component_1k")], results: one("ae2:matter_ball"), note: tr("gui.tooltips.ae2.MatterBalls", "Condense Into Matter Balls\n%s per item").replace("%s", "256") },
        Ae2Recipe { slots: vec![one("ae2:cell_component_64k")], results: one("ae2:singularity"), note: tr("gui.tooltips.ae2.Singularity", "Condense Into Singularities\n%s per item").replace("%s", "256,000") },
    ];
    Category { icon: "ae2:condenser", title: tr("block.ae2.condenser", "Matter Condenser"), catalysts: &["ae2:condenser"], layout: Layout::Single, width: 1, height: 1, recipes }
}

/// A fluid as AE2's JEI integration shows it: its still texture, animated (the content mod's `hz_fluid_*` model on
/// a bucket's stack), named as the fluid.
fn fluid(id: &str, name: &str) -> Vec<Stack> {
    vec![Stack {
        item: "minecraft:bucket".to_owned(),
        count: 1,
        components: format!("[minecraft:item_model=\"ae2:hz_fluid_{id}\",minecraft:item_name=\"{name}\"]"),
    }]
}

/// The fluids an ME network stores and how each is got. Horizon's fluids are the game's own two (water and lava);
/// a mod's fluid would join this list once Horizon lets mods add fluids.
fn fluids(game: &Game<'_>) -> Category {
    let step = |slots: Vec<Vec<Stack>>, results: Vec<Stack>, note: &str| Ae2Recipe { slots, results, note: note.to_owned() };
    let (water, lava) = (fluid("water", "Water"), fluid("lava", "Lava"));
    let mut recipes = vec![
        step(vec![one("minecraft:water_bucket")], water.clone(), "Empty a bucket (into the world, a tank or an ME terminal)"),
        step(vec![water.clone(), water.clone()], water.clone(), "Two sources beside each other make an endless source"),
        step(vec![one("minecraft:ice")], water.clone(), "Ice melts by light (not in the Nether)"),
        step(vec![one("minecraft:cauldron")], water.clone(), "Rain or snowfall fills a cauldron"),
        step(vec![water.clone(), one("minecraft:bucket")], one("minecraft:water_bucket"), "Fill a bucket from a source, a tank or an ME terminal"),
        step(vec![one("minecraft:lava_bucket")], lava.clone(), "Empty a bucket (into the world, a tank or an ME terminal)"),
        step(vec![one("minecraft:pointed_dripstone"), one("minecraft:cauldron")], lava.clone(), "Dripstone under lava drips it into a cauldron below"),
        step(vec![lava.clone(), one("minecraft:bucket")], one("minecraft:lava_bucket"), "Fill a bucket from a source, a tank or an ME terminal"),
    ];
    // AE2's entropy manipulator: what it turns into water or lava, and what they become.
    for r in game.native_recipes_of_type("ae2:entropy") {
        let json = &*r.json;
        let id_of = |v: Option<&serde_json::Value>| {
            let v = v?;
            v.get("block").or_else(|| v.get("fluid"))?.get("id")?.as_str().map(|s| s.replace("flowing_", ""))
        };
        let (input, output) = (id_of(json.get("input")), id_of(json.get("output")));
        let as_stack = |id: &str| match id {
            "minecraft:water" => water.clone(),
            "minecraft:lava" => lava.clone(),
            other => one(other),
        };
        if let (Some(i), Some(o)) = (input, output)
            && (i.ends_with(":water") || i.ends_with(":lava") || o.ends_with(":water") || o.ends_with(":lava"))
        {
            let mode = json.get("mode").and_then(|m| m.as_str()).unwrap_or("heat");
            recipes.push(step(vec![as_stack(&i)], as_stack(&o), &format!("Entropy Manipulator ({mode})")));
        }
    }
    // Stored by fluid cells in drives and ME chests, as an item network stores items.
    for size in ["1k", "4k", "16k", "64k", "256k"] {
        recipes.push(step(vec![water.clone()], one(&format!("ae2:fluid_storage_cell_{size}")), "Stored in a fluid cell"));
    }
    Category { icon: "minecraft:bucket", title: "Fluids".to_owned(), catalysts: &["minecraft:bucket"], layout: Layout::Grid, width: 0, height: 0, recipes }
}
});
