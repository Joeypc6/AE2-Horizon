fn ae2_panel_clear(game: &Game<'_>, layout: Option<Layout>) -> Option<Layout> {
    let Some(mut l) = layout else { return None };
    let extra = crate::game::port_ae2_me::panel_clearance(game);
    if extra <= 0.0 {
        return Some(l);
    }
    let right = l.area[0] + l.area[2];
    let left = l.area[0] + extra;
    let columns = (((right - left) / SLOT).floor().max(0.0) as usize).min(COLUMNS.0);
    if columns < COLUMNS.1 {
        return None;
    }
    let x = (left + (right - left - columns as f32 * SLOT) / 2.0).floor();
    l.area = [left, l.area[1], right - left, l.area[3]];
    l.grid.0 = x;
    l.columns = columns;
    l.search[0] = x;
    l.search[2] = columns as f32 * SLOT;
    Some(l)
}
