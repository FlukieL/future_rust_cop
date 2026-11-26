//! Cache-only startup must never compile or create a cache when it is missing.
use fcop::discover::{Code, Program};
use fcop::image::{Image, Section};
use fcop::jit::{compile_cached, load_cached_only};
use fcop::smc::Smc;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn missing_cache_fails_without_creating_one() {
    const TEXT: u32 = 0x401000;
    let img = Image {
        data: vec![0xc3],
        base: 0x400000,
        entry: TEXT,
        sections: vec![Section { name: ".text".into(), va: TEXT, vsize: 1, raw: 0, rsize: 1 }],
    };
    let mut code = Code::new(&img);
    let prog = Program::build(&mut code, &[(TEXT, TEXT + 1, "return".into())]);
    let smc = Smc::find(&prog, &mut code);
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcop-cache-missing-{}-{nonce}", std::process::id()));
    assert!(load_cached_only(&prog, &smc, &mut code, &[], (&path, [0; 32])).is_none());
    assert!(!path.exists());
}

#[test]
fn valid_cache_loads_and_wrong_key_is_rejected() {
    const TEXT: u32 = 0x401000;
    let img = Image {
        data: vec![0xc3],
        base: 0x400000,
        entry: TEXT,
        sections: vec![Section { name: ".text".into(), va: TEXT, vsize: 1, raw: 0, rsize: 1 }],
    };
    let mut code = Code::new(&img);
    let prog = Program::build(&mut code, &[(TEXT, TEXT + 1, "return".into())]);
    let smc = Smc::find(&prog, &mut code);
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fcop-cache-test-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("compiled.bin");
    let key = [42; 32];
    let compiled = compile_cached(&prog, &smc, &mut code, &[], Some((&path, key)));
    assert!(!compiled.stats.cached);
    drop(compiled);
    assert!(load_cached_only(&prog, &smc, &mut code, &[], (&path, [43; 32])).is_none());
    let loaded = load_cached_only(&prog, &smc, &mut code, &[], (&path, key)).expect("valid cache");
    assert!(loaded.stats.cached);
    drop(loaded);
    // A valid header with no function blobs is not a usable cache.
    let mut truncated = std::fs::read(&path).unwrap();
    truncated.truncate(44);
    truncated[40..44].copy_from_slice(&0u32.to_le_bytes());
    std::fs::write(&path, truncated).unwrap();
    assert!(load_cached_only(&prog, &smc, &mut code, &[], (&path, key)).is_none());
    // A corrupt count must be rejected without trying to allocate billions of entries.
    let mut oversized = b"FCOPJIT1".to_vec();
    oversized.extend_from_slice(&key);
    oversized.extend_from_slice(&u32::MAX.to_le_bytes());
    std::fs::write(&path, oversized).unwrap();
    assert!(load_cached_only(&prog, &smc, &mut code, &[], (&path, key)).is_none());
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
