//! AE2's integrations are compiled in only when the mod they work with is in the same build. A native mod is compiled on
//! the player's computer with the mods they have, so this is decided each time the launcher builds the game. It reads one
//! file, the game's own `src/game.rs` as staged, to see whether another bundle added that mod's module, and nothing else.

use proc_macro::{Delimiter, Group, TokenStream, TokenTree};

fn in_build(name: &str) -> bool {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let game = std::fs::read_to_string(format!("{dir}/src/game.rs")).unwrap_or_default();
    game.contains(&format!("mod {name} {{"))
}

/// `"name", { kept } [else { otherwise }]`.
fn parts(input: TokenStream) -> (String, TokenStream, TokenStream) {
    let mut tokens = input.into_iter();
    let name = match tokens.next() {
        Some(TokenTree::Literal(l)) => l.to_string().trim_matches('"').to_owned(),
        _ => panic!("if_mod: the mod's name first"),
    };
    assert!(matches!(tokens.next(), Some(TokenTree::Punct(p)) if p.as_char() == ','), "if_mod: a comma after the name");
    let kept = match tokens.next() {
        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => g.stream(),
        _ => panic!("if_mod: a block after the name"),
    };
    let otherwise = match (tokens.next(), tokens.next()) {
        (Some(TokenTree::Ident(i)), Some(TokenTree::Group(g))) if i.to_string() == "else" => g.stream(),
        (None, None) => TokenStream::new(),
        _ => panic!("if_mod: only `else {{ }}` may follow the block"),
    };
    (name, kept, otherwise)
}

fn choose(input: TokenStream) -> TokenStream {
    let (name, kept, otherwise) = parts(input);
    if in_build(&name) { kept } else { otherwise }
}

/// Items, kept or left out: `if_mod_items!("jei", { ... });`.
#[proc_macro]
pub fn if_mod_items(input: TokenStream) -> TokenStream {
    choose(input)
}

/// A block's value: `if_mod!("jei", { ... } else { ... })`.
#[proc_macro]
pub fn if_mod(input: TokenStream) -> TokenStream {
    TokenTree::Group(Group::new(Delimiter::Brace, choose(input))).into()
}
