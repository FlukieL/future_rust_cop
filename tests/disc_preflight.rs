//! Disc-image preflight must not modify the selected source.
use fcop::setup::{discover_launcher_images, inspect_game_image, inspect_ps1_music_image, install_launcher_image, install_launcher_music};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn invalid_and_missing_images_are_rejected_without_modifying_source() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fcop-disc-test-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let image = dir.join("invalid.iso");
    let original = b"not a disc image";
    std::fs::write(&image, original).unwrap();

    assert!(inspect_game_image(&image).is_err());
    assert!(inspect_ps1_music_image(&image).is_err());
    assert_eq!(std::fs::read(&image).unwrap(), original);
    assert_eq!(discover_launcher_images(&dir).unwrap(), (None, None));
    assert_eq!(std::fs::read(&image).unwrap(), original);

    let missing = dir.join("missing.iso");
    assert!(inspect_game_image(&missing).is_err());
    assert!(inspect_ps1_music_image(&missing).is_err());
    assert!(!missing.exists());

    let destination = dir.join("destination");
    assert!(install_launcher_image(&image, &destination).is_err());
    assert!(install_launcher_music(&image, &destination).is_err());
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&image).unwrap(), original);

    std::fs::remove_file(image).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
#[ignore = "requires FCOP_TEST_ISO pointing to a supported game disc"]
fn supported_disc_installs_without_overwriting_or_changing_source() {
    let image = std::path::PathBuf::from(std::env::var_os("FCOP_TEST_ISO").expect("set FCOP_TEST_ISO"));
    let info = inspect_game_image(&image).expect("supported PC disc");
    let (detected, _) = discover_launcher_images(image.parent().expect("image directory")).expect("scan image directory");
    assert_eq!(detected.as_deref(), Some(image.as_path()));
    let original = std::fs::metadata(&image).unwrap();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcop-install-test-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&root).unwrap();

    let exe = install_launcher_image(&image, &root).expect("install disc");
    assert_eq!(fcop::setup::check(&exe).unwrap().name, info.version);
    assert!(root.join("FCopLAPD").join("febmp.bin").is_file());
    assert!(root.join("FCopLAPD").join("missions").is_dir());
    assert!(install_launcher_image(&image, &root).is_err());
    let after = std::fs::metadata(&image).unwrap();
    assert_eq!(after.len(), original.len());
    assert_eq!(after.modified().ok(), original.modified().ok());
    std::fs::remove_dir_all(root).unwrap();
}
