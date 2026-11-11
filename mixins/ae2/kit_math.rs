ae2_optional::if_kit_jei_items!("ae2", {
// The sources' arithmetic: loot table number providers, world generation heights and chances, and how the pages
// write numbers. Nothing here touches the game, so the JEI fork's unit tests compile this file on its own.

use serde_json::Value;

/// A number provider's middle, or a plain number.
pub(super) fn average(v: &Value) -> f64 {
    if let Some(n) = v.as_f64() {
        return n;
    }
    if let Some(n) = v.get("value").and_then(Value::as_f64) {
        return n;
    }
    if let (Some(n), Some(p)) = (v.get("n").and_then(Value::as_f64), v.get("p").and_then(Value::as_f64)) {
        return n * p;
    }
    match (v.get("min").map(average), v.get("max").map(average)) {
        (Some(a), Some(b)) => (a + b) / 2.0,
        _ => 1.0,
    }
}

/// A count provider's least and most above 0, and the chance it is above 0.
pub(super) fn range(v: &Value) -> (u32, u32, f64) {
    if let Some(n) = v.as_f64() {
        return (n.max(0.0) as u32, n.max(0.0) as u32, 1.0);
    }
    if let Some(n) = v.get("value").and_then(Value::as_f64) {
        return (n.max(0.0) as u32, n.max(0.0) as u32, 1.0);
    }
    if let (Some(n), Some(p)) = (v.get("n").and_then(Value::as_f64), v.get("p").and_then(Value::as_f64)) {
        return (0, n as u32, 1.0 - (1.0 - p).powf(n));
    }
    let bound = |key: &str| v.get(key).map_or(1.0, average).floor() as i64;
    let (low, high) = (bound("min"), bound("max"));
    if low >= 1 {
        return (low as u32, high.max(low) as u32, 1.0);
    }
    // A uniform count of integers: how often it is 1 or more.
    let high = high.max(0);
    (1.min(high) as u32, high as u32, high as f64 / (high - low + 1) as f64)
}

/// A height anchor as a Y in the dimension.
pub(super) fn anchor(v: &Value, (bottom, height): (i32, i32)) -> i32 {
    if let Some(y) = v.get("absolute").and_then(Value::as_i64) {
        y as i32
    } else if let Some(y) = v.get("above_bottom").and_then(Value::as_i64) {
        bottom + y as i32
    } else if let Some(y) = v.get("below_top").and_then(Value::as_i64) {
        bottom + height - 1 - y as i32
    } else {
        bottom
    }
}

/// How a placed ore's veins spread over Y: the chance of each Y from the dimension's bottom up.
pub(super) fn height_shape(placement: &[Value], world: (i32, i32)) -> Vec<f64> {
    let mut out = vec![0.0; world.1 as usize];
    let Some(h) = placement.iter().find_map(|p| (p.get("type")?.as_str()? == "minecraft:height_range").then(|| p.get("height"))?) else {
        out.fill(1.0 / f64::from(world.1));
        return out;
    };
    let kind = h.get("type").and_then(Value::as_str).unwrap_or("minecraft:uniform");
    let low = h.get("min_inclusive").map_or(world.0, |a| anchor(a, world));
    let high = h.get("max_inclusive").map_or(world.0 + world.1 - 1, |a| anchor(a, world));
    let width = (high - low).max(0);
    let mut sum = 0.0;
    for y in low..=high {
        let weight = if kind == "minecraft:trapezoid" {
            let plateau = h.get("plateau").and_then(Value::as_i64).unwrap_or(0) as f64;
            let middle = f64::from(low + high) / 2.0;
            let half = f64::from(width) / 2.0;
            let d = (f64::from(y) - middle).abs();
            if d <= plateau / 2.0 { 1.0 } else { (half - d + 1.0).max(0.0) / (half - plateau / 2.0 + 1.0).max(1.0) }
        } else if kind.contains("biased") {
            1.0 / (1.0 + f64::from(y - low))
        } else {
            1.0
        };
        sum += weight;
        if let Some(i) = usize::try_from(y - world.0).ok().filter(|i| *i < out.len()) {
            out[i] = weight;
        }
    }
    if sum > 0.0 {
        out.iter_mut().for_each(|w| *w /= sum);
    }
    out
}

/// A placed feature's tries per chunk.
pub(super) fn tries(placement: &[Value]) -> f64 {
    let mut n = 1.0;
    for p in placement {
        match p.get("type").and_then(Value::as_str).unwrap_or("") {
            "minecraft:count" => n *= p.get("count").map_or(1.0, |c| match c.get("min_inclusive") {
                Some(lo) => (average(lo) + c.get("max_inclusive").map_or(0.0, average)) / 2.0,
                None => average(c),
            }),
            "minecraft:rarity_filter" => n /= p.get("chance").and_then(Value::as_f64).unwrap_or(1.0).max(1.0),
            _ => {}
        }
    }
    n
}

/// The blocks a vein of `size` places on average: 0.6 of its size, measured from generated chunks.
pub(super) fn vein_blocks(size: f64) -> f64 {
    size * 0.6
}

/// How much of a layer can hold ore in this dimension. The Overworld has bedrock under Y -59, and above Y 40 stone
/// gives way to air, leaving only mountains (measured from generated chunks).
pub(super) fn solid(d: usize, y: i32) -> f64 {
    if d != 0 {
        return 1.0;
    }
    if y < -59 {
        return f64::from(y + 64) / 5.0;
    }
    (f64::from(120 - y) / 80.0).clamp(0.02, 1.0)
}

/// "iron_ore" as "Iron Ore".
pub(super) fn title_case(text: &str) -> String {
    text.split(['_', '/', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + c.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A chance as JER shows it: "12.5%".
pub(super) fn percent(chance: f64) -> String {
    let p = chance * 100.0;
    if p >= 99.95 {
        "100%".to_owned()
    } else if p >= 10.0 {
        format!("{p:.0}%")
    } else if p >= 0.1 {
        format!("{p:.1}%")
    } else if p >= 0.01 {
        format!("{p:.2}%")
    } else {
        format!("{p:.3}%")
    }
}

/// A count without a trailing ".0".
pub(super) fn trim_count(n: f64) -> String {
    if n >= 10.0 { format!("{n:.0}") } else { format!("{n:.1}") }
}
});
