ae2_optional::if_kit_jei_items!("ae2", {
// The page flow over the original JEI mod, which it leaves as it is. JEI shows vanilla's recipes; this layer hears the
// same panel events, asks a JEI viewer of its own (JEI's public `Viewer`) for vanilla's recipe pages, adds what each
// mod's provider gives after them, then Information, Mob Drops, World Gen and the like, and shows the merged page in
// the window on the next frame.
use crate::game::Game;
use crate::game::jei::{self, Context, Event, Ingredient, Item, Kind, Layout, Out, Recipe, Stack, Station, View, Viewer};
use crate::game::native_hooks::{NativePanelEvent, NativeRecipeKind, NativeRecipes, NativeRecipeStack};
use crate::game::hz_kit_window_ae2::{Act, Card, Tab, Transfer};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

/// The Crafting tab's icon.
const CRAFTING: &str = "minecraft:crafting_table";

/// What a mod adds to the pages. Every field has a "nothing" (`Provider::new`).
#[derive(Clone, Copy)]
pub struct Provider {
    pub id: &'static str,
    /// What U on this item opens whole: it is the catalyst of one of the mod's categories.
    pub is_catalyst: fn(&str) -> bool,
    /// More blocks that do a vanilla station's recipes, by the station's icon.
    pub station_catalysts: fn(&str) -> &'static [&'static str],
    /// More recipes for the Crafting tab of a page (`uses`: the item's uses).
    pub crafting_views: fn(&Stack, bool) -> Vec<View>,
    /// The mod's tabs for a page, after vanilla's.
    pub tabs: fn(&Game<'_>, &Stack, bool) -> Vec<Tab>,
    /// How an item comes from the mod's world generation.
    pub sources: fn(&Game<'_>, &str) -> Vec<Tab>,
    /// What to say of an item no table gives.
    pub note: fn(&str) -> Option<&'static str>,
    /// One of the mod's screens takes recipes ("+").
    pub takes_recipes: fn(&Game<'_>) -> bool,
    /// What a recipe's "+" says on that screen.
    pub transfer_state: fn(&Game<'_>, &View) -> Option<Transfer>,
    /// Marks the recipes that get a "+" there.
    pub mark_recipes: fn(&Game<'_>, &mut Vec<Tab>),
    /// The "+" pressed there.
    pub plus: fn(&mut Game<'_>, usize),
    /// A click on this stack opens its recipes even when JEI's clicks give items.
    pub opens_on_click: fn(&Stack) -> bool,
}

impl Provider {
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            is_catalyst: |_| false,
            station_catalysts: |_| &[],
            crafting_views: |_, _| Vec::new(),
            tabs: |_, _, _| Vec::new(),
            sources: |_, _| Vec::new(),
            note: |_| None,
            takes_recipes: |_| false,
            transfer_state: |_, _| None,
            mark_recipes: |_, _| {},
            plus: |_, _| {},
            opens_on_click: |_| false,
        }
    }
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
    /// The panel's list as JEI's search leaves it (JEI's `Clicked` indexes it).
    listed: Vec<Stack>,
    names: std::collections::HashMap<String, String>,
    history: Vec<Page>,
    /// How many of the shown page's tabs are vanilla's.
    vanilla_tabs: usize,
    /// The world's recipes, for a catalyst's whole category.
    catalog: Arc<NativeRecipes>,
}

thread_local! {
    static LAYER: RefCell<Option<Layer>> = const { RefCell::new(None) };
    static PROVIDERS: RefCell<Vec<Provider>> = const { RefCell::new(Vec::new()) };
}

/// A mod's provider; each mod calls this every frame, and only the first time does anything.
pub fn register(provider: Provider) {
    PROVIDERS.with_borrow_mut(|p| {
        if !p.iter().any(|x| x.id == provider.id) {
            p.push(provider);
        }
    });
}

fn providers() -> Vec<Provider> {
    PROVIDERS.with_borrow(Clone::clone)
}

pub fn provider_sources(game: &Game<'_>, item: &str) -> Vec<Tab> {
    providers().iter().flat_map(|p| (p.sources)(game, item)).collect()
}

pub fn provider_note(item: &str) -> Option<&'static str> {
    providers().iter().find_map(|p| (p.note)(item))
}

/// The panel's list, for a provider that adds to it.
pub fn set_listed(list: Vec<Stack>) {
    LAYER.with_borrow_mut(|l| {
        if let Some(l) = l.as_mut() {
            l.listed = list;
        }
    });
}

/// The world's names, for a provider's search.
pub fn with_names<R>(f: impl FnOnce(&std::collections::HashMap<String, String>) -> R) -> Option<R> {
    LAYER.with_borrow(|l| l.as_ref().map(|l| f(&l.names)))
}

/// `Component.translatable`.
pub fn tr(key: &str, english: &str) -> String {
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
pub fn ensure(game: &mut Game<'_>) -> bool {
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
    // JEI 0.2 builds its viewer with the tables' variants (none here: the kit lists items, not subtypes); 0.1 without.
    let (query, out) = ae2_optional::if_has!("fn with_variants(", {
        Viewer::with_variants(items, recipes, Vec::new(), lang)
    } else {
        Viewer::new(items, recipes, lang)
    });
    let listed = match out {
        Out::Items(stacks) => stacks,
        _ => Vec::new(),
    };
    LAYER.with_borrow_mut(|l| {
        *l = Some(Layer { query, listed, names, history: Vec::new(), vanilla_tabs: 0, catalog });
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

fn takes_recipes(game: &Game<'_>) -> bool {
    providers().iter().any(|p| (p.takes_recipes)(game))
}

/// What a recipe's "+" says on the open screen; none on a screen no mod takes recipes on.
pub fn transfer_state(game: &Game<'_>, view: &View) -> Option<Transfer> {
    providers().iter().find_map(|p| (p.transfer_state)(game, view))
}

/// `native_panel_event` on JEI's panel: follow the player through recipe pages as JEI does, with the mods' tabs.
pub fn event(game: &mut Game<'_>, mod_id: &str, event: &NativePanelEvent) {
    if mod_id != jei::ID || !game.native_on(jei::ID) || !ensure(game) {
        return;
    }
    let (recipe_key, uses_key) = keys(game);
    let known = game.native_known_recipes();
    let ctx = Context { recipe_key: &recipe_key, uses_key: &uses_key, give: false, creative: false, known: &known };
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
                open(game, stack, *key == uses_key, &ctx);
            } else if crate::game::hz_kit_window_ae2::guide_hotkey(game).is_some_and(|h| *key == h) {
                // The guide: its key over an item, the panel's too.
                crate::game::hz_kit_window_ae2::guide_open(game, &stack.item);
            }
        }
        NativePanelEvent::Clicked { index, right } => {
            let stack = LAYER.with_borrow(|l| l.as_ref().and_then(|l| l.listed.get(*index as usize).cloned()));
            // In "click gives items" mode JEI gives it (or opens it in survival); otherwise its recipes. What a
            // provider added to the list is its own, so JEI has nothing to give: its recipe.
            let own = stack.as_ref().is_some_and(|s| providers().iter().any(|p| (p.opens_on_click)(s)));
            if jei::give(game.native_setting(jei::ID, "give").as_deref()) && !own {
                return;
            }
            if let Some(stack) = stack {
                open(game, stack, *right, &ctx);
            }
        }
        _ => {}
    }
}

/// Opens an item's recipes or uses, when anything has any (JEI says when nothing does).
fn open(game: &Game<'_>, item: Stack, uses: bool, ctx: &Context<'_>) {
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
    show(game, &page, ctx);
}

/// A vanilla category's title and the blocks it is made on, from its tab's icon.
fn vanilla_tab(icon: &str) -> (&'static str, Vec<&'static str>) {
    let (title, own): (&'static str, &'static [&'static str]) = match icon {
        CRAFTING => ("Crafting", &[CRAFTING]),
        "minecraft:furnace" => ("Smelting", &["minecraft:furnace"]),
        "minecraft:blast_furnace" => ("Blasting", &["minecraft:blast_furnace"]),
        "minecraft:smoker" => ("Smoking", &["minecraft:smoker"]),
        "minecraft:campfire" => ("Campfire Cooking", &["minecraft:campfire", "minecraft:soul_campfire"]),
        "minecraft:stonecutter" => ("Stonecutting", &["minecraft:stonecutter"]),
        "minecraft:smithing_table" => ("Smithing", &["minecraft:smithing_table"]),
        "minecraft:brewing_stand" => ("Brewing", &["minecraft:brewing_stand"]),
        _ => ("Recipes", &[]),
    };
    let mut all = own.to_vec();
    for p in providers() {
        all.extend_from_slice((p.station_catalysts)(icon));
    }
    (title, all)
}

/// A catalyst, of a mod's categories or vanilla's.
fn is_catalyst(item: &str) -> bool {
    providers().iter().any(|p| (p.is_catalyst)(item)) || Station::ALL.iter().any(|s| vanilla_tab(s.icon()).1.contains(&item))
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

/// Vanilla's tabs for a page (from the layer's own JEI viewer), then the mods', then where it comes from.
fn page_of(game: &Game<'_>, page: &Page, ctx: &Context<'_>) -> Shown {
    let providers = providers();
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
        // JEI's catalysts: U on a station, or on what a mod says does its work, has its whole category.
        if page.uses {
            for station in Station::ALL {
                if vanilla_tab(station.icon()).1.contains(&page.item.item.as_str()) {
                    add_to_tab(&mut tabs, station.icon(), station_views(&l.catalog, station));
                }
            }
        }
        for p in &providers {
            add_to_tab(&mut tabs, CRAFTING, (p.crafting_views)(&page.item, page.uses));
        }
        for p in &providers {
            tabs.extend((p.tabs)(game, &page.item, page.uses));
        }
        if !page.uses
            && let Some(text) = crate::game::hz_kit_window_ae2::guide_summary(game, &page.item.item)
        {
            let lines = crate::game::hz_kit_collection_ae2::wrap_text(game, &text);
            tabs.push(Tab { icon: "minecraft:book".to_owned(), title: "Information".to_owned(), catalysts: Vec::new(), cards: vec![Card::Text(lines)] });
        }
        // Where it comes from when not crafted, or what a spawn egg's mob drops and who trades for it.
        tabs.extend(if page.uses { crate::game::hz_kit_collection_ae2::use_tabs(game, &page.item.item) } else { crate::game::hz_kit_collection_ae2::got_tabs(game, &page.item.item) });
        let name = l.names.get(&page.item.item).cloned().unwrap_or_else(|| page.item.item.clone());
        Shown { title: format!("{}: {name}", if page.uses { "Uses" } else { "Recipes" }), tabs, vanilla: vanilla_tabs }
    })
}

/// The merged page, in the window.
fn show(game: &Game<'_>, page: &Page, ctx: &Context<'_>) {
    let mut shown = page_of(game, page, ctx);
    if shown.tabs.is_empty() {
        return;
    }
    let tab = page.tab.min(shown.tabs.len() - 1);
    for p in providers() {
        (p.mark_recipes)(game, &mut shown.tabs);
    }
    LAYER.with_borrow_mut(|l| {
        if let Some(l) = l.as_mut() {
            l.vanilla_tabs = shown.vanilla;
        }
    });
    crate::game::hz_kit_window_ae2::show(shown.title, shown.tabs, tab);
}

/// Empties the pages behind the window (a provider's "+" went back to the screen).
pub fn clear_history() {
    LAYER.with_borrow_mut(|l| {
        if let Some(l) = l.as_mut() {
            l.history.clear();
        }
    });
}

/// Once a frame: what the player did on the window, and the panel keeps its items.
pub fn frame(game: &mut Game<'_>) {
    if game.native_player().is_none() {
        LAYER.with_borrow_mut(|l| *l = None);
        crate::game::hz_kit_window_ae2::close();
        return;
    }
    crate::game::hz_kit_collection_ae2::frame(game);
    crate::game::hz_kit_creative_ae2::frame(game);
    if crate::game::native_panel::hz_kit_panel_ae2::has_recipes(game) {
        game.native_panel_recipes(jei::ID, "", Vec::new(), 0, Vec::new());
    }
    // The window goes with its screen.
    if matches!(game.screen, crate::game::Screen::Playing) && crate::game::hz_kit_window_ae2::is_open() {
        crate::game::hz_kit_window_ae2::close();
        clear_history();
    }
    crate::game::hz_kit_window_ae2::frame(game);
    let acts = crate::game::hz_kit_window_ae2::take_acts();
    if acts.is_empty() {
        return;
    }
    let (recipe_key, uses_key) = keys(game);
    let known = game.native_known_recipes();
    let ctx = Context { recipe_key: &recipe_key, uses_key: &uses_key, give: false, creative: false, known: &known };
    for act in acts {
        match act {
            Act::Tab(tab) => go_to_tab(game, &ctx, |_| tab),
            Act::NextTab(by) => {
                let n = crate::game::hz_kit_window_ae2::tab_count().max(1) as i32;
                go_to_tab(game, &ctx, |now| (now as i32 + by).rem_euclid(n) as usize);
            }
            Act::Page(by) => crate::game::hz_kit_window_ae2::turn_page(by, crate::game::hz_kit_window_ae2::cards_area(game)),
            Act::Slot { stack, uses } => open(game, Stack { count: 1, ..stack }, uses, &ctx),
            Act::Key { key, stack } => {
                let stack = Stack { count: 1, ..stack };
                if key == recipe_key || key == uses_key {
                    open(game, stack, key == uses_key, &ctx);
                } else if crate::game::hz_kit_window_ae2::guide_hotkey(game).is_some_and(|h| key == h) {
                    crate::game::hz_kit_window_ae2::guide_open(game, &stack.item);
                }
            }
            Act::Close => {
                crate::game::hz_kit_window_ae2::close();
                clear_history();
            }
            Act::Back => {
                let page = LAYER.with_borrow_mut(|l| {
                    let l = l.as_mut()?;
                    l.history.pop();
                    l.history.last().cloned()
                });
                match page {
                    Some(page) => show(game, &page, &ctx),
                    None => crate::game::hz_kit_window_ae2::close(),
                }
            }
            Act::Plus(index) => plus(game, index, &ctx),
            // A clickable area on a screen: the category, as U on its catalyst has it.
            Act::Category(item) => {
                if game.native_on(jei::ID) && ensure(game) {
                    open(game, Stack::item(&item), true, &ctx);
                }
            }
        }
    }
}

fn go_to_tab(game: &Game<'_>, ctx: &Context<'_>, tab: impl Fn(usize) -> usize) {
    let page = LAYER.with_borrow_mut(|l| {
        let top = l.as_mut()?.history.last_mut()?;
        top.tab = tab(top.tab);
        Some(top.clone())
    });
    if let Some(page) = page {
        show(game, &page, ctx);
    }
}

/// The "+": on a screen a mod takes recipes on, that mod's; elsewhere the game's recipe book places it.
fn plus(game: &mut Game<'_>, index: usize, ctx: &Context<'_>) {
    let Some(page) = LAYER.with_borrow(|l| l.as_ref().and_then(|l| l.history.last().cloned())) else { return };
    let vanilla = LAYER.with_borrow(|l| l.as_ref().map_or(0, |l| l.vanilla_tabs));
    if takes_recipes(game) {
        for p in providers() {
            if (p.takes_recipes)(game) {
                (p.plus)(game, index);
                return;
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
});
