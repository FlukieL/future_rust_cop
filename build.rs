// macOS opens windows only on the main thread, so the game runs there: give it the 256 MB
// stack the other systems' game thread gets (main.rs).
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bin=fcop=-Wl,-stack_size,0x10000000");
    }
}
