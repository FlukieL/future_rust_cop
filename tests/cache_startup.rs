//! Cache-only startup must not run game-file setup without an explicit installed executable.
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn maintenance_commands_without_game_path_exit_without_creating_files() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fcop-cache-startup-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();

    for command in ["--cache-only", "--prepare", "--validate-cache"] {
        let result = Command::new(env!("CARGO_BIN_EXE_fcop"))
            .current_dir(&dir)
            .env("FCOP_BATCH", "1")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run fcop");

        assert_eq!(result.status.code(), Some(2), "{command}: {}", String::from_utf8_lossy(&result.stderr));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }
    std::fs::remove_dir(dir).unwrap();
}
