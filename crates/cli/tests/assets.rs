//! End-to-end tests for `pack-assets`, `list-assets` and `unpack-assets`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tpt-press"))
        .args(args)
        .output()
        .expect("failed to run tpt-press")
}

fn path_str(p: &Path) -> &str {
    p.to_str().expect("test paths are valid UTF-8")
}

/// Builds a small asset tree and returns its root.
fn asset_tree(dir: &Path) -> PathBuf {
    let root = dir.join("assets");
    fs::create_dir_all(root.join("textures")).unwrap();
    fs::create_dir_all(root.join("levels/outdoor")).unwrap();
    fs::write(root.join("textures/rock.dds"), noise(30_000, 1)).unwrap();
    fs::write(
        root.join("levels/outdoor/map.json"),
        "{\"x\":1}\n".repeat(400),
    )
    .unwrap();
    fs::write(root.join("empty.bin"), b"").unwrap();
    root
}

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn pack_list_and_unpack_round_trip() {
    let dir = TempDir::new().unwrap();
    let root = asset_tree(dir.path());
    let archive = dir.path().join("game.tpa");
    let out_dir = dir.path().join("restored");

    let packed = run(&["pack-assets", path_str(&root), path_str(&archive)]);
    assert!(packed.status.success(), "{packed:?}");
    assert!(stdout(&packed).contains("packed 3 assets"));

    let listed = run(&["list-assets", path_str(&archive), "--verify"]);
    assert!(listed.status.success(), "{listed:?}");
    let text = stdout(&listed);
    assert!(text.contains("textures/rock.dds"), "{text}");
    assert!(text.contains("levels/outdoor/map.json"), "{text}");
    assert!(text.contains("all 3 checksums match"), "{text}");

    let unpacked = run(&["unpack-assets", path_str(&archive), path_str(&out_dir)]);
    assert!(unpacked.status.success(), "{unpacked:?}");

    for rel in ["textures/rock.dds", "levels/outdoor/map.json", "empty.bin"] {
        assert_eq!(
            fs::read(root.join(rel)).unwrap(),
            fs::read(out_dir.join(rel)).unwrap(),
            "{rel}"
        );
    }
}

#[test]
fn store_flag_keeps_assets_raw() {
    let dir = TempDir::new().unwrap();
    let root = asset_tree(dir.path());
    let archive = dir.path().join("raw.tpa");

    assert!(run(&[
        "pack-assets",
        path_str(&root),
        path_str(&archive),
        "--store"
    ])
    .status
    .success());

    let listed = run(&["list-assets", path_str(&archive)]);
    let text = stdout(&listed);
    assert!(!text.contains("zstd"), "{text}");
    assert!(text.contains("raw"), "{text}");
}

#[test]
fn damaged_archive_fails_verification() {
    let dir = TempDir::new().unwrap();
    let root = asset_tree(dir.path());
    let archive = dir.path().join("game.tpa");
    assert!(run(&[
        "pack-assets",
        path_str(&root),
        path_str(&archive),
        "--store"
    ])
    .status
    .success());

    let mut bytes = fs::read(&archive).unwrap();
    bytes[4096 + 100] ^= 0x20;
    fs::write(&archive, bytes).unwrap();

    let listed = run(&["list-assets", path_str(&archive), "--verify"]);
    assert!(!listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stderr).contains("checksum mismatch"));
}

#[test]
fn list_rejects_a_non_assets_file() {
    let dir = TempDir::new().unwrap();
    let plain = dir.path().join("plain.bin");
    fs::write(&plain, b"not an assets archive, but long enough to read").unwrap();
    let listed = run(&["list-assets", path_str(&plain)]);
    assert!(!listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stderr).contains("bad magic"));
}
