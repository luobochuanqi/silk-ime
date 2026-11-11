//! 与 Go 侧 internal/ipc 共用 protocol/fixtures 的一致性测试。

use std::fs;
use std::path::PathBuf;

use silk_protocol::{decode, encode, Error, MAX_FRAME_SIZE, VERSION};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/fixtures")
}

fn json_files(dir: &str) -> Vec<PathBuf> {
    let dir = fixture_root().join(dir);
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("fixtures 目录存在")
        .map(|entry| entry.expect("目录项可读").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(body);
    frame
}

fn parse_hex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0, "frame_hex 长度必须为偶数");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("frame_hex 只含十六进制字符"))
        .collect()
}

#[test]
fn valid_fixtures_round_trip() {
    let files = json_files("valid");
    assert!(!files.is_empty(), "缺少合法样例");

    let mut seen = Vec::new();
    for path in files {
        let body = fs::read(&path).expect("读取 fixture");
        let decoded = decode(&frame(&body))
            .unwrap_or_else(|err| panic!("{} 解码失败: {err}", path.display()));
        seen.push(decoded.type_.clone());

        let reencoded = encode(&decoded).expect("重新编码失败");
        let again = decode(&reencoded).expect("二次解码失败");
        assert_eq!(again, decoded, "{} 往返语义不一致", path.display());
    }

    for required in [
        "hello",
        "welcome",
        "focus_in",
        "focus_out",
        "key",
        "action",
        "update",
        "commit",
        "caret",
        "show",
        "hide",
        "ping",
        "pong",
        "error",
        "bye",
    ] {
        assert!(
            seen.contains(&required.to_string()),
            "缺少 {required} 的合法样例"
        );
    }
}

#[test]
fn malformed_fixtures_are_rejected() {
    let files = json_files("malformed");
    assert!(files.len() >= 5, "畸形样例不足 5 个");

    for path in files {
        let raw = fs::read_to_string(&path).expect("读取 fixture");
        let fixture: serde_json::Value =
            serde_json::from_str(&raw).expect("fixture 本身必须是合法 JSON");
        let reason = fixture["reason"].as_str().expect("缺少 reason");
        let bytes = parse_hex(fixture["frame_hex"].as_str().expect("缺少 frame_hex"));
        assert!(
            decode(&bytes).is_err(),
            "{}（{reason}）应解码失败",
            path.display()
        );
    }
}

#[test]
fn oversized_length_is_rejected() {
    assert!(matches!(decode(&[0xff; 4]), Err(Error::FrameTooLarge(_))));

    let mut sink = Vec::new();
    let mut oversized = vec![0u8; 8];
    oversized[..4].copy_from_slice(&((MAX_FRAME_SIZE + 1) as u32).to_le_bytes());
    assert!(matches!(
        silk_protocol::write_frame(&mut sink, &oversized),
        Err(Error::FrameTooLarge(_))
    ));
}

#[test]
fn envelope_validation_rules() {
    let cases = [
        r#"{"v":2,"id":1,"type":"ping","payload":{"ts":1}}"#,
        r#"{"v":1,"id":1,"type":"bogus","payload":{}}"#,
        r#"{"v":1,"id":1,"type":"hide","payload":{}}"#,
        r#"{"v":1,"id":1,"type":"key","sid":"s","payload":{"down":true}}"#,
        r#"{"v":1,"id":1,"type":"ping","payload":[]}"#,
        r#"{"v":1,"id":1,"type":"ping"}"#,
        r#"{"v":1,"id":-1,"type":"ping","payload":{"ts":1}}"#,
    ];
    for body in cases {
        assert!(
            matches!(
                decode(&frame(body.as_bytes())),
                Err(Error::InvalidMessage(_))
            ),
            "{body}"
        );
    }
}

#[test]
fn unknown_fields_are_ignored() {
    let body = br#"{"v":1,"id":7,"type":"ping","payload":{"ts":12,"future":"x"},"extra":true}"#;
    let env = decode(&frame(body)).expect("未知字段应被忽略");
    assert_eq!(env.id, 7);
    assert_eq!(env.v, VERSION);
}

#[test]
fn arbitrary_bytes_never_panic() {
    // 伪随机字节流使用线性同余，保证确定性且不引入依赖。
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    for _ in 0..2000 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let len = (state >> 33) as usize % 256;
        let mut buf = Vec::with_capacity(len);
        for _ in 0..len {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            buf.push((state >> 40) as u8);
        }
        let _ = decode(&buf);
    }
}
