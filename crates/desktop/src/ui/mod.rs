pub mod chat;
pub mod editor;
pub mod markdown_view;
pub mod root;
pub mod settings_window;
pub mod sidebar;
pub mod theme;

/// Stable FNV-1a hash for turning string ids into GPUI element ids.
pub fn hash_id(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
