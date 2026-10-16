use super::super::*;
use super::helpers::*;
use std::io::Read;

#[test]
fn documented_format_ids_are_written() {
    let (dir, log) = temp_log("stability-format");
    let mut store = Store::create(&log).unwrap();
    store.append(rec("Thing", "k", false, &[])).unwrap();
    store.checkpoint().unwrap();
    drop(store);

    let mut superblock = [0u8; 12];
    std::fs::File::open(&log)
        .unwrap()
        .read_exact(&mut superblock)
        .unwrap();
    assert_eq!(&superblock[4..12], LOG_FORMAT);

    let mut join_magic = [0u8; 8];
    std::fs::File::open(Store::join_map_path(&log))
        .unwrap()
        .read_exact(&mut join_magic)
        .unwrap();
    assert_eq!(&join_magic, JOIN_FORMAT);

    let mut ckpt_magic = [0u8; 8];
    std::fs::File::open(Store::checkpoint_path(&log))
        .unwrap()
        .read_exact(&mut ckpt_magic)
        .unwrap();
    assert_eq!(&ckpt_magic, CHECKPOINT_FORMAT);

    let _ = std::fs::remove_dir_all(&dir);
}
