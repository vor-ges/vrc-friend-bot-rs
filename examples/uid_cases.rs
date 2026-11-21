//! tools/diff_extract_uid.py が作った入力(JSON配列)を extract_uid に通して、結果(JSON配列)を返す。
use vrc_friend_bot_rs::vrc::extract_uid;
fn main() {
    let input = std::fs::read_to_string("/tmp/uid_in.json").unwrap();
    let cases: Vec<String> = serde_json::from_str(&input).unwrap();
    let out: Vec<Option<String>> = cases.iter().map(|s| extract_uid(s)).collect();
    std::fs::write("/tmp/uid_rs.json", serde_json::to_string(&out).unwrap()).unwrap();
}
