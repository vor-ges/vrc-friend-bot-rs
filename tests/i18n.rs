//! test_local.py の「言語切り替え (/lang)」のうち、i18n単体で確認できる部分の移植。
//! 言語はグローバル状態なので、並列実行で干渉しないよう1つのテストにまとめる。
//! (bot/vrc 側の tr キーの網羅性テストは、bot.rs / vrc.rs を移植したあとに足す)
use vrc_friend_bot_rs::db::Db;
use vrc_friend_bot_rs::i18n::{self, table, Arg};
use vrc_friend_bot_rs::tr;

fn is_jp(c: char) -> bool {
    matches!(c, 'ぁ'..='ん' | 'ァ'..='ヶ' | '一'..='龠')
}

/// `{...}` を全部拾って並べ替えたもの (Python版 test の ph() と同じ)
fn ph(s: &str) -> Vec<String> {
    let mut v = vec![];
    let mut cur: Option<String> = None;
    for c in s.chars() {
        match (c, cur.as_mut()) {
            ('{', None) => cur = Some("{".into()),
            ('}', Some(b)) => {
                b.push('}');
                v.push(cur.take().unwrap());
            }
            (_, Some(b)) => b.push(c),
            _ => {}
        }
    }
    v.sort();
    v
}

#[test]
fn i18n_all() {
    // --- 訳テーブルの健全性 ---
    let en = table("en").unwrap();
    assert!(en.len() > 60);
    for (k, v) in en {
        assert_eq!(ph(k), ph(v), "placeholder mismatch: {k}");
        assert!(!v.chars().any(is_jp), "英訳に日本語が残っている: {k} -> {v}");
    }
    let mut keys: Vec<_> = en.iter().map(|(k, _)| *k).collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), en.len(), "キーが重複している");

    // --- 既定のjaは従来表示と同一 ---
    assert_eq!(i18n::get_lang(), "ja");
    assert_eq!(tr!("所有者のみ実行できます。"), "所有者のみ実行できます。");

    // --- 切り替え・フォールバック ---
    i18n::set_lang("en").unwrap();
    assert_eq!(tr!("{name} (グループ)", name = "G"), "G (Group)");
    assert_eq!(tr!("表に無い日本語"), "表に無い日本語");
    assert_eq!(tr!("{name} (グループ)"), "{name} (Group)"); // 引数なしは未整形のまま返す(例外にしない)
    assert_eq!(tr!("❌ {msg}", msg = "boom"), "❌ boom"); // {msg} という名前の書式引数も渡せる
    assert_eq!(tr!("{name} (グループ)", wrong = 1), "{name} (グループ)"); // 引数名が違っても原文で返す(通知を止めない)
    assert!(i18n::set_lang("xx").is_err());
    assert_eq!(i18n::get_lang(), "en");

    // --- 書式指定 ({h:02d} / {x:.1f}) と値の型 ---
    assert_eq!(tr!("{h:02d}時", h = 5), "05h");
    assert_eq!(tr!("{h}時台({c}回)", h = 21, c = 3), "21:00 (3x)");
    assert_eq!(
        tr!("{emoji} {k} **{pct:.1f}%** ({hours:.1f}時間)", emoji = "🟢", k = "active", pct = 66.666, hours = 1.25),
        "🟢 active **66.7%** (1.2 h)" // 1.25 -> 1.2 (Pythonと同じ: 2進数での厳密な値で丸める)
    );
    assert_eq!(
        tr!("**{name}** — {logins}回 / 最多 {peak}時台 / {hours:.1f}h", name = "Bob", logins = 7u32, peak = 22, hours = 3.04),
        "**Bob** — 7 logins / peak 22:00 / 3.0 h"
    );
    assert_eq!(tr!("…他{n}人", n = 12usize), "…and 12 more");
    assert_eq!(
        tr!("🗑️ **{name}** を削除しました (チャンネルは保持): <#{ch}>", name = "A", ch = 123456789012345678i64),
        "🗑️ Removed **A** (channel kept): <#123456789012345678>"
    );
    i18n::set_lang("ja").unwrap();
    assert_eq!(tr!("{h:02d}時", h = 5), "05時");
    assert_eq!(tr!("{h:02d}時", h = -5), "-5時"); // 符号つきゼロ埋め (Python: format(-5,'02d') == '-5')

    // --- 書式エンジン (str.format の互換部分) ---
    let f = |t: &str, a: &[(&str, Arg)]| i18n::format(t, a);
    assert_eq!(f("{{x}} {a}", &[("a", 1.into())]), Ok("{x} 1".into()));
    assert_eq!(f("{a:>5}", &[("a", 1.into())]), Err(())); // 未対応の指定は静かに諦める
    assert_eq!(f("{a", &[("a", 1.into())]), Err(()));
    assert_eq!(f("a}", &[("a", 1.into())]), Err(()));
    assert_eq!(f("{}", &[("a", 1.into())]), Err(())); // 位置引数は使わない (Pythonは IndexError)
    assert_eq!(f("{a:.2f}", &[("a", 3.into())]), Ok("3.00".into())); // intにも f が使える
    assert_eq!(f("{a:d}", &[("a", "s".into())]), Err(())); // 文字列に d は不可 (Pythonは ValueError)

    // --- resolve: ISO 639-1/2、大小文字・地域つき表記 ---
    for (c, want) in [("en", "en"), ("EN", "en"), (" eng ", "en"), ("en-US", "en"), ("en_GB", "en"), ("ja", "ja"), ("jpn", "ja"), ("JA", "ja"), ("ja-JP", "ja")] {
        assert_eq!(i18n::resolve(c), Some(want), "{c}");
    }
    for c in ["", " ", "e", "fr", "jp", "JP", "jpx", "english", "日本語", "-", "xx-en"] {
        assert_eq!(i18n::resolve(c), None, "{c:?}"); // jp はISO 639ではないので不可
    }

    // --- 永続化 ---
    let d = Db::memory().unwrap();
    assert_eq!(i18n::load(&d.c).unwrap(), "ja"); // 設定が無い(旧DB)ならja
    i18n::save(&d.c, "en").unwrap();
    i18n::set_lang("ja").unwrap();
    assert_eq!(i18n::load(&d.c).unwrap(), "en"); // 再起動後も保持
    i18n::save(&d.c, "zz").unwrap();
    assert_eq!(i18n::load(&d.c).unwrap(), "ja"); // 壊れた値はjaに戻す
    assert_eq!(i18n::load(&d.c).unwrap(), "ja"); // 再実行(冪等)
    i18n::set_lang("ja").unwrap();
}
