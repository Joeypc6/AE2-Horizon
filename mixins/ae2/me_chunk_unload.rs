fn ae2_me_chunk_unload(dimension: &str, chunk: [i32; 2]) {
    crate::game::port_ae2_me::chunk_logged(dimension, chunk, false);
}
