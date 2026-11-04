fn ae2_plane_enchantable(item: &str, components: &mut serde_json::Map<String, serde_json::Value>) {
    if item == "ae2:annihilation_plane" {
        components.insert("minecraft:enchantable".to_owned(), serde_json::json!({"value": 10}));
    }
}
