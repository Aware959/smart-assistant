use super::*;

/// 重复 emoji、空 emoji、普通 emoji 各一张，够覆盖键生成的三条分支。
fn fixture() -> StickerCatalog {
    StickerCatalog::from_set(
        "PackA",
        "包标题",
        &[
            ("😀".to_string(), "f1".to_string()),
            ("😀".to_string(), "f2".to_string()),
            ("   ".to_string(), "f3".to_string()),
            ("抱抱".to_string(), "f4".to_string()),
        ],
    )
}

/// 每个测试用独立目录（带 pid），避免并行跑时互相踩；目录不存在也顺手补上，
/// 直接 `fs::write` 到一个还没建出来的目录下只会拿到 NotFound。
fn temp_path(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("sa-sticker-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("创建测试临时目录");
    dir.join(name).to_string_lossy().into_owned()
}

#[test]
fn from_set_键取emoji且重复加序号() {
    let c = fixture();
    assert_eq!(c.set, "PackA");
    assert_eq!(c.title, "包标题");
    // 首张原样，第二张同 emoji 加序号，空 emoji 归到占位符，中文键原样保留。
    assert_eq!(c.file_id("😀"), Some("f1"));
    assert_eq!(c.file_id("😀2"), Some("f2"));
    assert_eq!(c.file_id("•"), Some("f3"));
    assert_eq!(c.file_id("抱抱"), Some("f4"));
    assert_eq!(c.stickers.len(), 4);
}

/// 键必须与入参顺序无关地确定下来——同一批贴纸拉两次应得到同一份键。
#[test]
fn from_set_键生成是确定的() {
    let pairs: Vec<(String, String)> = vec![
        ("😀".to_string(), "a".to_string()),
        ("😀".to_string(), "b".to_string()),
        ("😀".to_string(), "c".to_string()),
    ];
    assert_eq!(fixture_from(&pairs).keys(), fixture_from(&pairs).keys());
    let keys = fixture_from(&pairs).keys();
    assert_eq!(keys.len(), 3);
    assert!(keys.contains(&"😀".to_string()));
    assert!(keys.contains(&"😀2".to_string()));
    assert!(keys.contains(&"😀3".to_string()));
}

fn fixture_from(pairs: &[(String, String)]) -> StickerCatalog {
    StickerCatalog::from_set("PackA", "", pairs)
}

/// 文件缺失是正常状态（还没拉过），只关掉贴纸能力，绝不能让装配失败。
#[test]
fn 目录不存在按空目录处理() {
    let c = StickerCatalog::load(&temp_path("definitely-missing.json"));
    assert!(c.is_empty());
    assert!(c.keys().is_empty());
}

#[test]
fn 目录损坏按空目录处理() {
    let path = temp_path("broken.json");
    std::fs::write(&path, "{ 这不是 json").expect("写测试文件");
    let c = StickerCatalog::load(&path);
    assert!(c.is_empty(), "损坏的目录应退成空目录而不是 panic");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn save与load往返一致且会补建父目录() {
    let path = temp_path("sub/deep/roundtrip.json");
    let c = fixture();
    c.save(&path).expect("应能写入含缺失父目录的路径");
    assert_eq!(StickerCatalog::load(&path), c, "读回应与写入完全一致");
    let _ = std::fs::remove_file(&path);
}
