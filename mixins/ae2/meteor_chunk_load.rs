fn ae2_meteor_chunk_load(game: &mut crate::game::Game<'_>, dimension: &str, chunk: [i32; 2], new: bool) {
    crate::game::port_ae2_meteor::chunk_load(game, dimension, chunk, new);
}
