//! The language of what the platform posts (#588): the PR description,
//! the reply to a review round and the diagnosis report. The platform's own
//! words come from a fixed catalog per [`Locale`]; the model is asked to
//! write its parts in the same language, and its text is made inert the
//! same way whatever the language.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A language the platform writes in. `en` unless a repo says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Locale {
    #[default]
    En,
    Ja,
    Vi,
}

impl Locale {
    pub const ALL: [Locale; 3] = [Locale::En, Locale::Ja, Locale::Vi];

    /// The code stored and configured: `en`, `ja` or `vi`.
    pub fn as_str(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Ja => "ja",
            Locale::Vi => "vi",
        }
    }

    /// The language's name, as the model is told to write in it.
    pub fn language(self) -> &'static str {
        match self {
            Locale::En => "English",
            Locale::Ja => "Japanese",
            Locale::Vi => "Vietnamese",
        }
    }

    /// The platform's own words in this language.
    pub fn strings(self) -> &'static PrStrings {
        PrStrings::for_locale(self)
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A locale code that is not `en`, `ja` or `vi`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown locale `{0}`: expected en, ja or vi")]
pub struct UnknownLocale(pub String);

impl FromStr for Locale {
    type Err = UnknownLocale;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "en" => Ok(Locale::En),
            "ja" => Ok(Locale::Ja),
            "vi" => Ok(Locale::Vi),
            _ => Err(UnknownLocale(s.to_owned())),
        }
    }
}

/// Every fixed piece of text the platform posts, in one language. The
/// headings and labels are Markdown the platform writes itself; nothing in
/// here comes from the model.
#[derive(Debug)]
#[non_exhaustive]
pub struct PrStrings {
    // PR description.
    pub issue_heading: &'static str,
    pub cause_heading: &'static str,
    pub evidence_label: &'static str,
    pub changes_heading: &'static str,
    pub test_heading: &'static str,
    pub repro_test: &'static str,
    pub repro_passes: &'static str,
    pub none: &'static str,
    pub allowed_tests: &'static str,
    pub allowed_references: &'static str,
    pub updated_references: &'static str,
    pub files: &'static str,
    pub tests: &'static str,
    pub scope_warning: &'static str,
    pub scope_review: &'static str,
    pub flags_warning: &'static str,
    pub flags_review: &'static str,
    pub gate_heading: &'static str,
    pub cost_heading: &'static str,
    pub calls: &'static str,
    pub cut: &'static str,
    pub more: fn(usize) -> String,
    // Reply to a review round.
    pub pushed: fn(&str) -> String,
    /// The round ended without a commit; the argument is already inert.
    pub pushed_nothing: fn(&str) -> String,
    /// The review is not taken (#589); the argument is already inert.
    pub review_refused: fn(&str) -> String,
    /// A plan's approval is not taken (#675); the argument is already
    /// inert.
    pub approve_refused: fn(&str) -> String,
    pub line: &'static str,
    pub review: &'static str,
    pub comment: &'static str,
    pub not_done: &'static str,
    pub no_answer: &'static str,
    pub changes: &'static str,
    // Diagnosis report.
    pub diagnosis: fn(&str, f64) -> String,
    pub findings: &'static str,
    pub steps: &'static str,
    pub questions: &'static str,
    // The comment on the issue when its job ends (#589); each follows
    // `**aegis-code** ` or `aegis-code `.
    pub pull_request: &'static str,
    pub merge_request: &'static str,
    /// The request's kind, and its URL when it is a plain one.
    pub opened: fn(&str, Option<&str>) -> String,
    pub looked_into: fn(&str) -> String,
    pub platform_failed: &'static str,
    /// The request's kind and why it stopped.
    pub stopped: fn(&str, &str) -> String,
    pub budget_refused: &'static str,
    pub cancelled: &'static str,
    pub reason: &'static str,
    pub job: &'static str,
    pub stop_pr_opened: &'static str,
    pub stop_budget: &'static str,
    pub stop_steps: &'static str,
    pub stop_expert: &'static str,
    pub stop_diagnosed: &'static str,
    /// A plan waits for a person's approval (#675).
    pub stop_awaiting_approval: &'static str,
}

impl PrStrings {
    pub fn for_locale(locale: Locale) -> &'static PrStrings {
        match locale {
            Locale::En => &EN,
            Locale::Ja => &JA,
            Locale::Vi => &VI,
        }
    }
}

fn more_en(n: usize) -> String {
    format!("… and {n} more")
}
fn more_ja(n: usize) -> String {
    format!("… ほか {n} 件")
}
fn more_vi(n: usize) -> String {
    format!("… và {n} khác")
}

fn pushed_en(commit: &str) -> String {
    format!("**aegis-code** pushed `{commit}` for the review; the checks passed.")
}
fn pushed_ja(commit: &str) -> String {
    format!(
        "**aegis-code** がレビューに対応して `{commit}` を push しました。チェックはすべて通過しています。"
    )
}
fn pushed_vi(commit: &str) -> String {
    format!("**aegis-code** đã push `{commit}` theo review; các kiểm tra đều đạt.")
}

fn pushed_nothing_en(reason: &str) -> String {
    format!("**aegis-code** pushed nothing for this review: {reason}.")
}
fn pushed_nothing_ja(reason: &str) -> String {
    format!("**aegis-code** はこのレビューに対して何も push しませんでした: {reason}。")
}
fn pushed_nothing_vi(reason: &str) -> String {
    format!("**aegis-code** không push gì cho lần review này: {reason}.")
}

fn review_refused_en(why: &str) -> String {
    format!("**aegis-code** will not take this review: {why}.")
}
fn review_refused_ja(why: &str) -> String {
    format!("**aegis-code** はこのレビューに対応しません: {why}。")
}
fn review_refused_vi(why: &str) -> String {
    format!("**aegis-code** sẽ không nhận lần review này: {why}.")
}

fn approve_refused_en(why: &str) -> String {
    format!("**aegis-code** cannot start this plan yet: {why}.")
}
fn approve_refused_ja(why: &str) -> String {
    format!("**aegis-code** はこの計画をまだ開始できません: {why}。")
}
fn approve_refused_vi(why: &str) -> String {
    format!("**aegis-code** chưa thể bắt đầu kế hoạch này: {why}.")
}

fn diagnosis_en(category: &str, confidence: f64) -> String {
    format!(
        "**Diagnosis**: {category} (confidence {confidence:.2}). Read-only: nothing was changed."
    )
}
fn diagnosis_ja(category: &str, confidence: f64) -> String {
    format!("**診断**: {category} (確信度 {confidence:.2})。読み取りのみで、何も変更していません。")
}
fn diagnosis_vi(category: &str, confidence: f64) -> String {
    format!(
        "**Chẩn đoán**: {category} (độ tin cậy {confidence:.2}). Chỉ đọc: không có gì bị thay đổi."
    )
}

fn opened_en(request: &str, url: Option<&str>) -> String {
    match url {
        Some(url) => format!("opened a {request} for this issue: {url}"),
        None => format!("opened a {request} for this issue."),
    }
}
fn opened_ja(request: &str, url: Option<&str>) -> String {
    match url {
        Some(url) => format!("がこの issue の {request} を作成しました: {url}"),
        None => format!("がこの issue の {request} を作成しました。"),
    }
}
fn opened_vi(request: &str, url: Option<&str>) -> String {
    match url {
        Some(url) => format!("đã mở một {request} cho issue này: {url}"),
        None => format!("đã mở một {request} cho issue này."),
    }
}

fn looked_into_en(request: &str) -> String {
    format!(
        "looked into this issue: it does not read as a code defect, so no {request} was \
         opened. What it found is below; a person decides what to do."
    )
}
fn looked_into_ja(request: &str) -> String {
    format!(
        "がこの issue を調査しました。コードの不具合ではないと判断したため、{request} は\
         作成していません。調査結果は以下のとおりです。対応は担当者が判断してください。"
    )
}
fn looked_into_vi(request: &str) -> String {
    format!(
        "đã xem xét issue này: đây không giống lỗi trong code, nên không mở {request} nào. \
         Kết quả nằm bên dưới; người phụ trách sẽ quyết định cách xử lý."
    )
}

fn stopped_en(request: &str, why: &str) -> String {
    format!("stopped without a {request}: {why}.")
}
fn stopped_ja(request: &str, why: &str) -> String {
    format!("は {request} を作成せずに停止しました: {why}。")
}
fn stopped_vi(request: &str, why: &str) -> String {
    format!("đã dừng mà không mở {request}: {why}.")
}

static EN: PrStrings = PrStrings {
    issue_heading: "Issue",
    cause_heading: "Root cause",
    evidence_label: "Checked sources:",
    changes_heading: "Changes (by hunk)",
    test_heading: "Tests",
    repro_test: "Reproduction test",
    repro_passes: "(FAIL → PASS)",
    none: "none",
    allowed_tests: "Tests allowed to change",
    allowed_references: "Reference groups allowed to change:",
    updated_references: "Gate references updated, the reviewer should confirm:",
    files: "Files",
    tests: "Tests",
    scope_warning: "**Patch scope warning:**",
    scope_review: "Review carefully before approving the merge.",
    flags_warning: "**Text that reads like an instruction to an AI agent** \
                    (found by the platform in what the job read):",
    flags_review: "The agent not following these lines does not make them safe: \
                   check their source, as the repo or the issue may be targeted.",
    gate_heading: "Gate result",
    cost_heading: "Cost",
    calls: "calls",
    cut: "(cut)",
    more: more_en,
    pushed: pushed_en,
    pushed_nothing: pushed_nothing_en,
    review_refused: review_refused_en,
    approve_refused: approve_refused_en,
    line: "line",
    review: "review",
    comment: "comment",
    not_done: "not done",
    no_answer: "no answer",
    changes: "Changes:",
    diagnosis: diagnosis_en,
    findings: "Findings, each with the sources it was checked against:",
    steps: "Suggested steps for an operator (not applied):",
    questions: "Questions for the reporter:",
    pull_request: "pull request",
    merge_request: "merge request",
    opened: opened_en,
    looked_into: looked_into_en,
    platform_failed: "could not finish this issue: the platform failed. Its operators can see why.",
    stopped: stopped_en,
    budget_refused: "did not take this issue: the repository's monthly budget is spent.",
    cancelled: "cancelled its job for this issue.",
    reason: "Reason",
    job: "aegis-code job",
    stop_pr_opened: "a pull or merge request was opened",
    stop_budget: "its budget is spent",
    stop_steps: "it reached its step limit",
    stop_expert: "even the strongest model could not fix it",
    stop_diagnosed: "it was diagnosed as needing no code change",
    stop_awaiting_approval: "its plan waits for a person's approval",
};

static JA: PrStrings = PrStrings {
    issue_heading: "Issue",
    cause_heading: "根本原因",
    evidence_label: "確認したソース:",
    changes_heading: "変更内容 (ハンクごと)",
    test_heading: "テスト",
    repro_test: "再現テスト",
    repro_passes: "(FAIL → PASS)",
    none: "なし",
    allowed_tests: "変更を許可されたテスト",
    allowed_references: "変更を許可された参照グループ:",
    updated_references: "gate が更新した参照 (レビュアーの確認が必要):",
    files: "ファイル",
    tests: "テスト",
    scope_warning: "**パッチ範囲 (Patch Scope) の警告:**",
    scope_review: "マージを承認する前に十分に確認してください。",
    flags_warning: "**AI エージェントへの指示のように読める文章** \
                    (ジョブが読んだ内容からプラットフォームが検出):",
    flags_review: "エージェントがこれらの行に従わなかったとしても安全とは限りません。\
                   リポジトリや issue が標的にされている可能性があるため、出所を確認してください。",
    gate_heading: "Gate 結果",
    cost_heading: "コスト",
    calls: "回",
    cut: "(以下省略)",
    more: more_ja,
    pushed: pushed_ja,
    pushed_nothing: pushed_nothing_ja,
    review_refused: review_refused_ja,
    approve_refused: approve_refused_ja,
    line: "行",
    review: "レビュー",
    comment: "コメント",
    not_done: "未対応",
    no_answer: "回答なし",
    changes: "変更内容:",
    diagnosis: diagnosis_ja,
    findings: "所見 (それぞれ照合したソース付き):",
    steps: "運用担当者向けの推奨手順 (未適用):",
    questions: "報告者への質問:",
    pull_request: "pull request",
    merge_request: "merge request",
    opened: opened_ja,
    looked_into: looked_into_ja,
    platform_failed: "はこの issue を完了できませんでした。プラットフォームで障害が発生しました。\
                      原因は運用担当者が確認できます。",
    stopped: stopped_ja,
    budget_refused: "はこの issue を引き受けませんでした。リポジトリの月間予算を使い切っています。",
    cancelled: "はこの issue のジョブをキャンセルしました。",
    reason: "理由",
    job: "aegis-code ジョブ",
    stop_pr_opened: "pull request または merge request が作成された",
    stop_budget: "予算を使い切った",
    stop_steps: "ステップ上限に達した",
    stop_expert: "最上位のモデルでも修正できなかった",
    stop_diagnosed: "コード変更は不要と診断された",
    stop_awaiting_approval: "計画が承認待ちである",
};

static VI: PrStrings = PrStrings {
    issue_heading: "Issue",
    cause_heading: "Nguyên nhân",
    evidence_label: "Nguồn đã kiểm:",
    changes_heading: "Thay đổi (theo từng đoạn)",
    test_heading: "Test",
    repro_test: "Test tái hiện",
    repro_passes: "(FAIL → PASS)",
    none: "không có",
    allowed_tests: "Test được phép đổi",
    allowed_references: "Nhóm tham chiếu được phép đổi:",
    updated_references: "Tham chiếu gate đã cập nhật, người duyệt cần xác nhận:",
    files: "File",
    tests: "Test",
    scope_warning: "**Cảnh báo phạm vi patch (Patch Scope):**",
    scope_review: "Người review cần kiểm tra kỹ trước khi duyệt merge.",
    flags_warning: "**Văn bản giống lệnh gửi AI agent** \
                    (platform tự phát hiện trong những gì job đã đọc):",
    flags_review: "Agent không làm theo những dòng này không có nghĩa là an toàn: \
                   người review nên kiểm tra nguồn, vì repo hoặc issue có thể đang bị nhắm tới.",
    gate_heading: "Kết quả gate",
    cost_heading: "Chi phí",
    calls: "lượt",
    cut: "(đã cắt bớt)",
    more: more_vi,
    pushed: pushed_vi,
    pushed_nothing: pushed_nothing_vi,
    review_refused: review_refused_vi,
    approve_refused: approve_refused_vi,
    line: "dòng",
    review: "review",
    comment: "bình luận",
    not_done: "chưa làm",
    no_answer: "không có trả lời",
    changes: "Thay đổi:",
    diagnosis: diagnosis_vi,
    findings: "Các phát hiện, mỗi mục kèm nguồn đã đối chiếu:",
    steps: "Các bước gợi ý cho người vận hành (chưa áp dụng):",
    questions: "Câu hỏi cho người báo lỗi:",
    pull_request: "pull request",
    merge_request: "merge request",
    opened: opened_vi,
    looked_into: looked_into_vi,
    platform_failed: "không thể hoàn thành issue này: platform gặp lỗi. \
                      Người vận hành có thể xem nguyên nhân.",
    stopped: stopped_vi,
    budget_refused: "không nhận issue này: ngân sách tháng của repository đã hết.",
    cancelled: "đã hủy job của issue này.",
    reason: "Lý do",
    job: "job aegis-code",
    stop_pr_opened: "một pull request hoặc merge request đã được mở",
    stop_budget: "đã hết ngân sách",
    stop_steps: "đã chạm giới hạn số bước",
    stop_expert: "ngay cả model mạnh nhất cũng không sửa được",
    stop_diagnosed: "được chẩn đoán là không cần sửa code",
    stop_awaiting_approval: "kế hoạch đang chờ người duyệt",
};

/// Non-ASCII letters only Vietnamese uses; tests check English and
/// Japanese output has none.
#[cfg(test)]
pub(crate) fn has_vietnamese(text: &str) -> bool {
    text.chars().any(|c| {
        "ăâđêôơưàáạảãầấậẩẫằắặẳẵèéẹẻẽềếệểễìíịỉĩòóọỏõồốộổỗờớợởỡùúụủũừứựửữỳýỵỷỹ"
            .contains(c.to_lowercase().next().unwrap_or(c))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_and_default_to_english() {
        assert_eq!(Locale::default(), Locale::En);
        for l in Locale::ALL {
            assert_eq!(l.as_str().parse::<Locale>(), Ok(l));
            let json = serde_json::to_string(&l).unwrap();
            assert_eq!(json, format!("\"{}\"", l.as_str()));
            assert_eq!(serde_json::from_str::<Locale>(&json).unwrap(), l);
        }
        assert_eq!(" JA ".parse::<Locale>(), Ok(Locale::Ja));
        assert!("fr".parse::<Locale>().is_err());
    }

    #[test]
    fn only_the_vietnamese_catalog_is_vietnamese() {
        for l in [Locale::En, Locale::Ja] {
            let s = l.strings();
            let all = format!(
                "{s:?}{}{}{}{}{}{}{}{}{}",
                (s.more)(2),
                (s.pushed)("abc"),
                (s.pushed_nothing)("x"),
                (s.review_refused)("x"),
                (s.approve_refused)("x"),
                (s.opened)("pull request", Some("https://x")),
                (s.looked_into)("pull request"),
                (s.stopped)("pull request", "x"),
                (s.diagnosis)("x", 0.5)
            );
            assert!(!has_vietnamese(&all), "{l}: {all}");
        }
        assert!(has_vietnamese(Locale::Vi.strings().cause_heading));
    }
}
