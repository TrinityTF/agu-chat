//! Prints the key names and value lengths of the secrets.bin in AGU_DATA_DIR (a COPY, never the real data).
//! With `write`, stores a test secret first. Interop check against Electron's safeStorage.
use agu_chat_core::log::Logger;
use agu_chat_core::store::Store;

fn main() {
    let dir = agu_chat_core::default_data_dir();
    if std::env::var_os("AGU_DATA_DIR").is_none() {
        eprintln!("set AGU_DATA_DIR to a temp copy");
        std::process::exit(2);
    }
    let log = std::sync::Arc::new(Logger::new(&dir, true));
    let store = Store::open(dir, log);
    if std::env::args().nth(1).as_deref() == Some("write") {
        store.secrets.set("probe", serde_json::json!({"hello": "from rust"})).unwrap();
        println!("wrote probe");
    }
    for k in store.secrets.keys() {
        println!("key {k}: {} bytes of json", store.secrets.get(&k).map(|v| v.to_string().len()).unwrap_or(0));
    }
}
