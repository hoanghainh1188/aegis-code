//! The pages in Japanese (#590), by their English text: one arm per string
//! the templates pass to `crate::t`; a new string needs its arm here and in
//! the other catalog (`i18n::tests` checks both).

pub(super) fn get(en: &str) -> Option<&'static str> {
    Some(match en {
        "Pre-flight check" => "事前チェック",
        "Check" => "チェック",
        "Result" => "結果",
        "No pre-flight check yet. It checks the repo's profile, clone, lockfiles, sandbox image and fetch hosts, then runs its gate on the base, as" => {
            "まだ事前チェックはありません。リポジトリのプロファイル、クローン、ロックファイル、サンドボックスイメージ、取得先ホストを確認し、ベース上でゲートを実行します。これは"
        }
        "does." => "と同じです。",
        "Run the pre-flight check" => "事前チェックを実行",
        "Turn the repo on" => "リポジトリを有効にする",
        "Waiting for the first result: a check waits for a free slot, then runs the repo's gate on its base." => {
            "最初の結果を待っています。チェックは空きを待ってから、ベース上でリポジトリのゲートを実行します。"
        }
        "blocking" => "ブロック",
        "warn" => "警告",
        "skip" => "スキップ",
        "running" => "実行中",
        "passed" => "合格",
        "warned" => "警告あり",
        "failed" => "失敗",
        "the merge request" => "merge request",
        "the pull request" => "pull request",
        "Internal server error" => "サーバー内部エラーが発生しました",
        "% of cap; below" => "% です。残りが",
        "(ADR 0005): acceptance target ≥ 40% overall, ≥ 35% per language. Hidden test results are not shown here." => {
            "(ADR 0005)。承認率の目標は全体で 40% 以上、言語ごとに 35% 以上です。隠しテストの結果はここには表示されません。"
        }
        "(reserve per issue), repo accepts no new jobs; queued jobs wait for next month or until cap is raised." => {
            "(issue ごとの予備) を下回ると、リポジトリは新しいジョブを受け付けません。キュー内のジョブは翌月まで、または上限が引き上げられるまで待機します。"
        }
        "(service)" => "(サービス)",
        ", and set it in the configuration." => {
            "の部分を利用者がこのサーバーにアクセスするアドレスに置き換え、設定に追加してください。"
        }
        ", done" => "、完了:",
        ", for when this page cannot be reached. See the incident runbook." => {
            "(このページにアクセスできない場合)。インシデント対応手順書を参照してください。"
        }
        ", one per browser they signed in with." => "(サインインしたブラウザーごとに 1 件)。",
        ", or" => "、または",
        ", so with less than that left under the cap no new job runs; jobs already running finish." => {
            "を使う可能性があるため、上限までの残りがそれを下回ると新しいジョブは実行されません。実行中のジョブは完了します。"
        }
        ", then paste the token when prompted." => {
            "を実行し、求められたらトークンを貼り付けてください。"
        }
        ", which never comes through your browser. A webhook already pointing at this server is updated instead of doubled." => {
            "で署名され、この値がブラウザを経由することはありません。すでにこのサーバーを指している Webhook は、重複して作成せずに更新します。"
        }
        ", with the roles it has in each repo." => "。各リポジトリで持つロールの範囲で動作します。",
        ". A token does not end by itself: revoke one that is no longer used." => {
            "。トークンは自動では失効しません。使わなくなったものは失効させてください。"
        }
        ". It is used for this request only, then dropped: not kept, logged or recorded. The webhook is signed with the server's own" => {
            "。このリクエストにだけ使用され、その後破棄されます (保存・ログ出力・記録はされません)。Webhook はサーバー自身の"
        }
        ". No new job runs; queued jobs wait" => {
            "。新しいジョブは実行されず、キュー内のジョブは待機しています"
        }
        ". Queued jobs wait and new ones are recorded; none runs until the agents are resumed." => {
            "。キュー内のジョブは待機し、新しいジョブは記録されます。エージェントを再開するまでジョブは実行されません。"
        }
        ". Queued jobs wait and new ones are recorded; none runs until the repo is resumed." => {
            "。キュー内のジョブは待機し、新しいジョブは記録されます。リポジトリを再開するまでジョブは実行されません。"
        }
        ". To have the agent work on it again, use Request changes there or comment" => {
            "。エージェントに再度対応させるには、そこで Request changes を使うか、次のコメントを"
        }
        ": a fork (or the repo itself) and the secret of its token, below." => {
            ": フォーク (またはリポジトリ本体) と、そのトークンのシークレット (下記)。"
        }
        ": copy it now." => "。今すぐコピーしてください。",
        ": do that on" => "の承認・マージ・変更は行いません。それらは次で行ってください:",
        ": it is in paths and branch names, so it does not change." => {
            ": パスやブランチ名に使われるため、変更できません。"
        }
        ": the instance's GitHub App installed on the repo; the agent's" => {
            ": リポジトリにインストールされたこのインスタンスの GitHub App。エージェントの"
        }
        "; PR is reviewed there." => "(PR のレビューはそちらで行います)。",
        "; this signs you out too" => "。あなた自身もサインアウトされます",
        "A queued job is cancelled at once; a running one stops when its worker next checks in." => {
            "キュー内のジョブはすぐにキャンセルされます。実行中のジョブは、ワーカーが次に応答した時点で停止します。"
        }
        "AMS report" => "AMS レポート",
        "API tokens" => "API トークン",
        "Accepted" => "承認",
        "Accepted PRs" => "承認された PR",
        "Accepted with edits" => "修正して承認",
        "Account" => "アカウント",
        "Action" => "操作",
        "Active tokens" => "有効なトークン",
        "Add a repo" => "リポジトリを追加",
        "Add a service" => "サービスを追加",
        "Add an admin…" => "管理者を追加…",
        "Add and clone" => "追加してクローン",
        "Add service" => "サービスを追加",
        "Admin" => "管理者",
        "Admin right" => "管理者権限",
        "Admin ·" => "管理 ·",
        "Advisory" => "脆弱性情報",
        "All agents paused" => "すべてのエージェントが一時停止中",
        "Also revoke their personal tokens, so the" => "個人用トークンも失効させ、",
        "An admin may do everything in every repo, these pages included." => {
            "管理者は、これらのページを含め、すべてのリポジトリであらゆる操作ができます。"
        }
        "An admin may do everything in every repo, these pages included. A person may be named before they sign in for the first time; a name without" => {
            "管理者は、これらのページを含め、すべてのリポジトリであらゆる操作ができます。初回サインイン前の人も指定できます。"
        }
        "As a repo file: what" => "リポジトリファイルとして:",
        "As an admin they may do everything in every repo, whatever their roles." => {
            "管理者なので、ロールにかかわらず、すべてのリポジトリであらゆる操作ができます。"
        }
        "As the agent wrote it in its final answer; the gate checked the patch, not this text." => {
            "エージェントが最終回答に書いたとおりです。gate が確認したのはパッチであり、この文章ではありません。"
        }
        "Asks the forge's API, from the server, what each token may do and what the repo is called now." => {
            "サーバーからフォージの API に、各トークンで何ができるか、リポジトリの現在の名前は何かを問い合わせます。"
        }
        "Attempt" => "試行",
        "Attempts" => "試行",
        "Audit log" => "監査ログ",
        "Average review fix rounds per accepted PR" => {
            "承認された PR あたりの平均レビュー修正ラウンド数"
        }
        "Awaiting review" => "レビュー待ち",
        "Began (UTC)" => "開始 (UTC)",
        "Bias investigation flags (#511)" => "バイアス調査フラグ (#511)",
        "Bind" => "紐付け",
        "Broke tests" => "テストを壊した",
        "By contract and SLA level (#526). MTTR runs from the ticket coming in to the end of its job's last run, for the jobs that ended with a result. On time: of those with an SLA deadline, the ones that ended by it. Triage: resolved by a diagnosis on the ticket, with no PR." => {
            "契約と SLA レベル別です (#526)。MTTR は、結果が出て終了したジョブについて、チケットの受付からジョブの最後の実行の終了までを測ります。期限内: SLA 期限のあるもののうち、期限までに終了したもの。トリアージ: PR を作らず、チケット上の診断で解決したもの。"
        }
        "By job creation week" => "ジョブ作成週別",
        "By language" => "言語別",
        "By repo" => "リポジトリ別",
        "By tenant" => "テナント別",
        "By ticket language" => "チケットの言語別",
        "By ticket source" => "チケットの取得元別",
        "CI or a connector that calls the API: name it, make it a token on its page, and give it roles on the repos' pages." => {
            "API を呼び出す CI やコネクタです。名前を付け、そのページでトークンを作成し、各リポジトリのページでロールを付与します。"
        }
        "CLI stops working for them" => "CLI を使えなくする",
        "Cache read" => "キャッシュ読み取り",
        "Calls" => "呼び出し数",
        "Cancel its queued jobs too" => "キュー内のジョブもキャンセルする",
        "Cancel job…" => "ジョブをキャンセル…",
        "Cap" => "上限",
        "Cap (USD / month)" => "上限 (USD / 月)",
        "Change" => "変更",
        "Change the cap" => "上限を変更",
        "Change this repo" => "このリポジトリを変更",
        "Changes" => "変更内容",
        "Check against" => "確認先:",
        "Check now" => "今すぐ確認",
        "Check the connection" => "接続を確認",
        "Checked against its mode's configuration as" => {
            "モードの設定に照らして確認します (次と同様:"
        }
        "Clear" => "クリア",
        "Clone" => "クローン",
        "Clone URL" => "クローン URL",
        "Connection" => "接続方式",
        "Connection to" => "接続先:",
        "Connection to the forge" => "フォージへの接続",
        "Content type" => "Content type",
        "Contract" => "契約",
        "Conventions" => "規約",
        "Copy it now into the service's secret store; only its hash is kept here." => {
            "今すぐサービスのシークレットストアにコピーしてください。ここにはハッシュだけが保存されます。"
        }
        "Cost" => "コスト",
        "Cost / issue" => "issue あたりのコスト",
        "Cost per contract" => "契約ごとのコスト",
        "Create CVE job" => "CVE ジョブを作成",
        "Create job" => "ジョブを作成",
        "Create token" => "トークンを作成",
        "Created" => "作成",
        "Created (UTC)" => "作成 (UTC)",
        "Created at" => "作成日時",
        "Days" => "日数",
        "Default branch" => "デフォルトブランチ",
        "Delivery" => "配信",
        "Detail" => "詳細",
        "Diagnosed, no PR" => "診断のみ (PR なし)",
        "Diagnosis" => "診断",
        "Dimension" => "観点",
        "Done" => "完了",
        "E-mail or service" => "メールアドレスまたはサービス",
        "Each customer installs the app on their own org. Bind each installation to the tenant whose org it is: until then no token is minted for it, and once bound, only that tenant's repos get its tokens. A bound installation moves to another tenant only after it is unbound. The same as" => {
            "各顧客は自社の org にアプリをインストールします。各インストールを、その org を持つテナントに紐付けてください。紐付けるまではトークンは発行されず、紐付け後はそのテナントのリポジトリだけがトークンを受け取ります。紐付け済みのインストールを別のテナントへ移すには、先に紐付けを解除します。コマンドでは次と同じです:"
        }
        "Effort" => "推論量",
        "Embedded instructions in what the job read" => {
            "ジョブが読み込んだ内容に含まれる埋め込み指示"
        }
        "Embedded instructions the agent reported" => "エージェントが報告した埋め込み指示",
        "Emergency stop" => "緊急停止",
        "Emergency stop: running jobs are cancelled too" => {
            "緊急停止: 実行中のジョブもキャンセルする"
        }
        "Ended" => "終了",
        "Ends (UTC)" => "終了 (UTC)",
        "Error" => "エラー",
        "Error · aegis" => "エラー · aegis",
        "Escalate to an emergency stop…" => "緊急停止に切り替える…",
        "Escalated" => "エスカレーション",
        "Escalated to Expert" => "Expert へのエスカレーション",
        "Event" => "イベント",
        "Every job works on the code whatever Light read (#497). A ticket read as other than" => {
            "Light の判定にかかわらず、すべてのジョブはコードの修正に取り組みます (#497)。"
        }
        "Every registered repo. Open one to see its settings and clone, change it, and change who has which role in it." => {
            "登録済みのすべてのリポジトリです。開くと設定とクローンの確認、変更、メンバーのロール変更ができます。"
        }
        "Every running job is asked to cancel; each stops at its worker's next lease renewal." => {
            "実行中のすべてのジョブにキャンセルを要求します。各ジョブは、ワーカーが次にリースを更新する時点で停止します。"
        }
        "Every write, by whom and when: roles, repos, jobs, reviews, tokens, budgets. Newest first; it cannot be changed." => {
            "すべての書き込みを、誰がいつ行ったかとともに記録します: ロール、リポジトリ、ジョブ、レビュー、トークン、予算。新しい順で、変更はできません。"
        }
        "Existing review:" => "既存のレビュー:",
        "Expires" => "有効期限",
        "Filter" => "絞り込み",
        "Find" => "検索",
        "Flags" => "フラグ",
        "Flags appear once a slice reaches 5 finished attempts (ADR 0016)." => {
            "フラグは、区分ごとに完了した試行が 5 件に達すると表示されます (ADR 0016)。"
        }
        "For" => "用途",
        "For a lost device, an account at risk, or someone leaving. They can sign in again unless the SSO stops them" => {
            "端末の紛失、アカウントの危険、退職などのときに使います。SSO で止めない限り、再びサインインできます"
        }
        "For a token that leaked, or a service no longer used. What uses them stops at once." => {
            "トークンが漏えいしたときや、使わなくなったサービスに使います。それらを使っているものはすぐに止まります。"
        }
        "For an incident that is not one repo's: a model acting strangely, a prompt injection spreading, a provider billing wrong. Webhooks, trackers and Sonar still record jobs while paused; nothing in the queue is lost. To hold one repo, pause it from its own page." => {
            "特定のリポジトリに限らないインシデント向けです: モデルの異常な挙動、プロンプトインジェクションの拡散、プロバイダーの誤請求など。一時停止中も Webhook、トラッカー、Sonar はジョブを記録し続け、キューの内容は失われません。1 つのリポジトリだけを止めるには、そのリポジトリのページから一時停止してください。"
        }
        "For review rounds on the agent's pull requests." => {
            "エージェントの pull request のレビューラウンド用です。"
        }
        "Fork" => "フォーク",
        "Found by the platform, not reported by the agent. The repo or the issue may be targeted: check the source." => {
            "エージェントの報告ではなく、プラットフォームが検出したものです。リポジトリや issue が標的にされている可能性があります。出所を確認してください。"
        }
        "From" => "取得元",
        "GitHub App" => "GitHub App",
        "GitHub App installations" => "GitHub App のインストール",
        "Give a token that may manage webhooks there:" => {
            "そこで Webhook を管理できるトークンを入力してください:"
        }
        "Give role" => "ロールを付与",
        "Group" => "グループ",
        "Guard:" => "ガード:",
        "Held: no verifiable sources" => "保留: 検証できる根拠なし",
        "How far" => "停止の範囲",
        "How the platform reaches the repo" => "プラットフォームがリポジトリにアクセスする方法",
        "In US dollars a month;" => "米ドル/月で指定します。現在までの支出は",
        "In the store" => "ストア内",
        "Installation" => "インストール",
        "Installed, not registered" => "インストール済み・未登録",
        "Issue" => "issue",
        "Job" => "ジョブ",
        "Job #" => "ジョブ #",
        "Job budget" => "ジョブ予算",
        "Jobs" => "ジョブ",
        "Jobs at once" => "同時実行ジョブ数",
        "Key" => "キー",
        "Kind" => "種別",
        "Label" => "ラベル",
        "Label (machine, usage)" => "ラベル (マシン、用途)",
        "Language" => "言語",
        "Last seen (UTC)" => "最終利用 (UTC)",
        "Last webhooks" => "最近の Webhook",
        "Ledger" => "台帳",
        "Ledger history" => "台帳の履歴",
        "Ledger · all completed runs" => "台帳 · 完了したすべての実行",
        "Ledger · jobs created this month" => "台帳 · 今月作成されたジョブ",
        "Less than" => "残りが",
        "Level" => "レベル",
        "Light read the ticket as" => "Light によるチケットの判定",
        "Lines edited" => "編集した行数",
        "MTTR and SLA" => "MTTR と SLA",
        "Made (UTC)" => "作成 (UTC)",
        "Main" => "メイン",
        "Maintainers create, cancel and retry jobs; reviewers also review PRs; viewers read. A person is named by the e-mail the SSO gives, and may be given a role before signing in for the first time; a name without" => {
            "maintainer はジョブの作成・キャンセル・再試行ができ、reviewer はさらに PR をレビューでき、viewer は閲覧のみです。人は SSO が返すメールアドレスで指定し、初回サインイン前にロールを付与できます。"
        }
        "Make a token" => "トークンを作成",
        "Make admin" => "管理者にする",
        "Make admin…" => "管理者にする…",
        "Make someone an admin" => "管理者を追加",
        "Make the webhook" => "Webhook を作成",
        "Make them by hand on the forge, or let an admin make them from here with a token given once: set" => {
            "フォージ上で手動で作成するか、管理者が 1 回限りのトークンでここから作成できるようにしてください: サーバーの設定で"
        }
        "Make them from here" => "ここから作成する",
        "Mean cost" => "平均コスト",
        "Mode" => "モード",
        "Model" => "モデル",
        "Model calls" => "モデル呼び出し",
        "Model tiers" => "モデル階層",
        "Month tracked in" => "月の集計は",
        "Monthly budget" => "月間予算",
        "Monthly cap" => "月間上限",
        "My tokens" => "マイトークン",
        "Name" => "名前",
        "New job" => "新しいジョブ",
        "New token," => "新しいトークンです。",
        "Next page →" => "次のページ →",
        "No PR" => "PR なし",
        "No attempts yet." => "試行はまだありません。",
        "No clone made from these pages; one made with" => {
            "これらのページから作成したクローンはありません。次で作成したクローンは"
        }
        "No events in ledger yet." => "台帳にイベントはまだありません。",
        "No installation recorded yet: they arrive with the app's" => {
            "インストールの記録はまだありません。アプリの"
        }
        "No job created this month yet." => "今月作成されたジョブはまだありません。",
        "No jobs match the filter." => "条件に一致するジョブはありません。",
        "No live session: they are signed out everywhere." => {
            "有効なセッションはありません。すべての場所でサインアウトしています。"
        }
        "No live token." => "有効なトークンはありません。",
        "No one" => "該当者なし",
        "No one has a role in this repo yet." => {
            "このリポジトリにはまだ誰もロールを持っていません。"
        }
        "No repo registered yet." => "登録済みのリポジトリはまだありません。",
        "No repos yet." => "リポジトリはまだありません。",
        "No role in any repo. Give one on the repo's page." => {
            "どのリポジトリにもロールがありません。リポジトリのページで付与してください。"
        }
        "No runs yet." => "実行はまだありません。",
        "No tokens yet." => "トークンはまだありません。",
        "No webhook from this repo yet. GitHub sends a ping when the webhook is made; GitLab's \"Test\" sends a sample event." => {
            "このリポジトリからの Webhook はまだありません。GitHub は Webhook の作成時に ping を送信し、GitLab では「Test」でサンプルイベントを送信できます。"
        }
        "None of its own" => "独自の上限なし",
        "Not configured: the server's" => "未設定: サーバーの",
        "Note" => "コメント",
        "Nothing" => "該当なし",
        "Older →" => "古い記録 →",
        "On" => "対象",
        "On the fork" => "フォーク上",
        "On time" => "期限内",
        "Open PR" => "PR を開く",
        "Open PR in new tab" => "PR を新しいタブで開く",
        "Open, overdue" => "未完了・期限超過",
        "Other settings" => "その他の設定",
        "Outcome" => "結果",
        "Owner" => "オーナー",
        "PR broke existing tests" => "PR が既存のテストを壊した",
        "PRs opened" => "作成した PR",
        "PRs without review" => "レビューのない PR",
        "Package" => "パッケージ",
        "Pause all" => "すべて一時停止",
        "Pause all agents" => "すべてのエージェントを一時停止",
        "Pause all agents…" => "すべてのエージェントを一時停止…",
        "Pause or resume" => "一時停止・再開",
        "Pause the repo" => "リポジトリを一時停止",
        "Pause the repo when it runs wrong or spends too fast: its queued jobs wait, and new ones are still recorded, until it is resumed. Jobs already running finish; cancel one from its own page." => {
            "リポジトリの動作がおかしいときや費用の消化が速すぎるときに一時停止します。再開するまで、キュー内のジョブは待機し、新しいジョブは引き続き記録されます。実行中のジョブは完了します。個別に止めるには各ジョブのページからキャンセルしてください。"
        }
        "Paused" => "一時停止中",
        "Payload URL" => "Payload URL",
        "People (by the e-mail the SSO gives) and services (API tokens). Open one to see their roles and change the admin right; roles in a repo are given on the repo's page." => {
            "人 (SSO が返すメールアドレスで識別) とサービス (API トークン) です。開くとロールの確認と管理者権限の変更ができます。リポジトリのロールはリポジトリのページで付与します。"
        }
        "Platform" => "プラットフォーム",
        "Provider" => "プロバイダー",
        "Raise a dependency for an advisory" => "脆弱性情報に合わせて依存関係を更新",
        "Received (UTC)" => "受信 (UTC)",
        "Recent webhooks" => "最近の Webhook",
        "Rejected" => "却下",
        "Remaining" => "残り",
        "Remove the cap" => "上限を削除",
        "Replace existing review" => "既存のレビューを置き換える",
        "Replies to the review" => "レビューへの返信",
        "Repo" => "リポジトリ",
        "Repos" => "リポジトリ",
        "Reproduction test:" => "再現テスト:",
        "Resume" => "再開",
        "Resume all agents…" => "すべてのエージェントを再開…",
        "Resume the repo" => "リポジトリを再開",
        "Retry as a new job" => "新しいジョブとして再試行",
        "Review PR" => "PR をレビュー",
        "Review fix round for PR opened by" => {
            "PR のレビュー修正ラウンドです。PR を作成したジョブ:"
        }
        "Review fix rounds" => "レビュー修正ラウンド",
        "Review job #" => "レビュー: ジョブ #",
        "Review rounds / PR" => "PR あたりのレビューラウンド",
        "Review time" => "レビュー時間",
        "Review time / PR" => "PR あたりのレビュー時間",
        "Review time is calculated automatically from opening this page until submission." => {
            "レビュー時間は、このページを開いてから送信するまでの時間として自動で計算されます。"
        }
        "Revoke" => "失効させる",
        "Revoke every token…" => "すべてのトークンを失効させる…",
        "Role" => "ロール",
        "Roles" => "ロール",
        "Round" => "ラウンド",
        "Running" => "実行中",
        "Runs" => "実行数",
        "Runs again" => "再実行:",
        "Same formulas as" => "計算式は次と同じです:",
        "Save and fetch" => "保存して取得",
        "Save cap" => "上限を保存",
        "Secret" => "シークレット",
        "Secrets" => "シークレット",
        "Sends one short request to the light model; it costs a little, so once a minute at most." => {
            "light モデルに短いリクエストを 1 件送ります。少し費用がかかるため、最大で 1 分に 1 回です。"
        }
        "Sessions" => "セッション",
        "Settings" => "設定",
        "Show" => "表示",
        "Sign in" => "サインイン",
        "Sign in with SSO" => "SSO でサインイン",
        "Sign in · aegis" => "サインイン · aegis",
        "Sign in:" => "サインイン:",
        "Sign out" => "サインアウト",
        "Sign out everywhere…" => "すべての場所でサインアウト…",
        "Since (UTC)" => "付与日時 (UTC)",
        "Slug" => "スラッグ",
        "Spent" => "支出",
        "Spent this month" => "今月の支出",
        "Started" => "開始",
        "State" => "状態",
        "Status" => "状態",
        "Stop taking jobs: running jobs finish" => "ジョブの受付を停止: 実行中のジョブは完了させる",
        "Stopped" => "停止",
        "Stopped without PR" => "PR なしで停止",
        "Submit review" => "レビューを送信",
        "TOML, as" => "TOML 形式 (リポジトリファイルの",
        "Take away" => "取り消す",
        "Take the admin right…" => "管理者権限を外す…",
        "Tenant" => "テナント",
        "Termination reason:" => "終了理由:",
        "The GitHub App is installed on these repos; registering one still takes its language, mode and limits." => {
            "これらのリポジトリには GitHub App がインストールされています。登録するには、言語、モード、上限を指定する必要があります。"
        }
        "The end of git's output:" => "git の出力の末尾:",
        "The model provider of each mode, as the server loaded it from the mode's file at start. Keys stay in the secret store: only their names, and whether the store has them, show here. Changing a provider or a model is a change to the mode's file and a restart." => {
            "各モードのモデルプロバイダーです。サーバーが起動時にモードのファイルから読み込んだ内容を表示します。キーはシークレットストアに保管されたままで、ここには名前とストアにあるかどうかだけが表示されます。プロバイダーやモデルを変えるには、モードのファイルを変更してサーバーを再起動します。"
        }
        "The month is counted in the" => "月の集計は",
        "The name the token has in the secret store, never the token itself." => {
            "シークレットストア内でのトークンの名前です。トークンそのものは入力しないでください。"
        }
        "The new token, shown this once." => "新しいトークンです。表示はこの 1 回だけです。",
        "The patch was truncated here; the PR has it whole." => {
            "パッチはここで省略されています。全体は PR で確認できます。"
        }
        "The platform raises the package with the repo's upgrade command; the agent only adapts the code if the checks fail (ADR 0015)." => {
            "プラットフォームがリポジトリのアップグレードコマンドでパッケージを更新し、チェックが失敗した場合にのみエージェントがコードを修正します (ADR 0015)。"
        }
        "The same as" => "次と同じです:",
        "The server's" => "サーバーの",
        "The service sends it as" => "サービスはこれを",
        "They keep their roles in each repo. The last admin cannot lose the right." => {
            "各リポジトリのロールはそのまま残ります。最後の管理者は権限を失いません。"
        }
        "This is you. Another admin can take your admin right; the last admin keeps it." => {
            "これはあなたです。別の管理者はあなたの管理者権限を外せますが、最後の管理者は権限を失いません。"
        }
        "This repo's audit log" => "このリポジトリの監査ログ",
        "This server has not loaded the modes' configuration: add repos with" => {
            "このサーバーはモードの設定を読み込んでいません。リポジトリは次で追加してください:"
        }
        "Tier" => "階層",
        "Token" => "トークン",
        "Token, used once" => "トークン (1 回のみ使用)",
        "Token: name of the secret" => "トークン: シークレットの名前",
        "Tokens" => "トークン",
        "Tokens for the" => "お使いのマシンの",
        "Total PR" => "PR 合計",
        "Triage" => "トリアージ",
        "Triage miss" => "トリアージの見落とし",
        "Triage, recorded only" => "トリアージ (記録のみ)",
        "UTC. They name no repo; if they are yours, the webhook's secret is not the value of" => {
            "UTC)。リポジトリは特定できません。心当たりがある場合、Webhook のシークレットが次の値と一致していません:"
        }
        "Unable to load page" => "ページを読み込めません",
        "Unbind" => "紐付けを解除",
        "Usage" => "使用率",
        "Users" => "ユーザー",
        "Verdict" => "判定",
        "Version" => "バージョン",
        "Wall time" => "経過時間",
        "Webhooks to make" => "作成する Webhook",
        "What became of the forge's last" => "フォージから届いた直近",
        "What the agent did" => "エージェントが行ったこと",
        "What they did, in the audit log" => "監査ログでの操作履歴",
        "When (UTC)" => "日時 (UTC)",
        "Where" => "場所",
        "Where the agent pushes and opens pull requests, on the" => {
            "エージェントが push して pull request を作成する場所 (接続方式"
        }
        "Whether the forge can reach this server and the tokens can do their part, without labelling a real issue to try." => {
            "実際の issue にラベルを付けて試さなくても、フォージがこのサーバーに到達できるか、トークンが必要な操作を行えるかを確認できます。"
        }
        "Whether the secret store has each one; their values are never shown." => {
            "シークレットストアにそれぞれが存在するかどうかです。値は表示されません。"
        }
        "Who" => "実行者",
        "Why (optional)" => "理由 (任意)",
        "Why, and what to do" => "理由と対処",
        "Workers claim the queued jobs again at once, oldest first, within each repo's limits. Check the cause is fixed first." => {
            "ワーカーはすぐにキュー内のジョブを古い順に、各リポジトリの上限内で再び取得します。先に原因が解消されたことを確認してください。"
        }
        "Written back to the ticket instead of a pull request (#451). Read-only: aegis changed nothing; each finding was checked against the source it cites." => {
            "pull request の代わりにチケットへ書き戻しました (#451)。読み取りのみで aegis は何も変更していません。各所見は、引用元のソースと照合済みです。"
        }
        "Yes, cancel job #" => "はい、ジョブをキャンセルします #",
        "Yes, make" => "はい、次のユーザーを",
        "Yes, pause all agents" => "はい、すべてのエージェントを一時停止します",
        "Yes, resume all agents" => "はい、すべてのエージェントを再開します",
        "Yes, revoke every token of" => "はい、次のすべてのトークンを失効させます:",
        "Yes, sign" => "はい、",
        "Yes, stop every running job" => "はい、実行中のすべてのジョブを停止します",
        "Yes, take it from" => "はい、次のユーザーから外します:",
        "Your verdict is recorded for the pilot's measures: acceptance, the lines you edited and your review time. It does not approve, merge or change the" => {
            "あなたの判定は、パイロットの指標 (承認、編集した行数、レビュー時間) として記録されます。"
        }
        "active tokens, valid for up to" => "件まで、有効期間はそれぞれ最大",
        "added to the agent's instructions" => "エージェントへの指示に追加されます",
        "admin" => "管理者",
        "all" => "すべて",
        "an admin" => "管理者にします",
        "and" => "と",
        "and running ones were asked to cancel" => "。実行中のジョブにはキャンセルを要求しました",
        "any" => "すべて",
        "at" => "日時:",
        "branches and PRs go to the repo itself and the fields below stay empty." => {
            "ブランチと PR はリポジトリ本体に作成され、下の欄は空のままです。"
        }
        "budget" => "予算",
        "by" => "実行者:",
        "checks it. Its clone is made after, in the background; the repo's page shows how it goes." => {
            "と同じ確認)。クローンはその後バックグラウンドで作成され、進み具合はリポジトリのページに表示されます。"
        }
        "choose…" => "選択…",
        "client on your machine: acts as you, with your exact permissions on each repo, recorded under your name in audit logs. Up to" => {
            "クライアント用のトークンです。あなたとして、各リポジトリでのあなたと同じ権限で動作し、監査ログにはあなたの名前で記録されます。有効なトークンは最大"
        }
        "connection page" => "接続ページ",
        "connection." => "のとき)。",
        "days each." => "日です。",
        "deliveries for this repo, kept 30 days: the job each made or found, or why it made none. Deliveries with a bad signature name no repo; see the" => {
            "件の配信 (30 日間保持) の結果です: それぞれが作成または検出したジョブ、作成しなかった場合はその理由。署名が不正な配信はリポジトリを特定できないため、次を参照してください:"
        }
        "does not show here." => "ここには表示されません。",
        "done" => "対応済み",
        "events." => "イベントで届きます。",
        "fail" => "失敗",
        "files, as the gate checked them." => "ファイル (gate が確認した状態)。",
        "for these filters" => "(この条件)",
        "has no file for it, so no repo can use it." => {
            "にこのモードのファイルがないため、どのリポジトリも使えません。"
        }
        "hours, the last at" => "時間、最後は",
        "in the server's configuration." => "を設定します。",
        "investigate > 30%" => "30% 超なら調査",
        "is a service." => "を含まない名前はサービスです。",
        "is left under this cap, so no new job of this repo runs: queued jobs wait until next month, or until the cap is raised." => {
            "を下回ったため、このリポジトリの新しいジョブは実行されません。キュー内のジョブは翌月まで、または上限が引き上げられるまで待機します。"
        }
        "is not set: put the address people reach this server at in place of" => {
            "が設定されていません。"
        }
        "is spent so far. Leave it empty for no cap of the repo's own, so that only each job's cap and the mode's budget apply." => {
            "です。空欄にするとリポジトリ独自の上限はなくなり、ジョブごとの上限とモードの予算だけが適用されます。"
        }
        "job" => "ジョブ",
        "job #" => "ジョブ #",
        "live session" => "件の有効なセッション",
        "live sessions" => "件の有効なセッション",
        "live token" => "件の有効なトークン",
        "live tokens" => "件の有効なトークン",
        "matches" => "(検索条件に一致)",
        "me" => "自分",
        "missing" => "未設定",
        "model acts strangely" => "モデルの挙動がおかしい",
        "name or e-mail" => "名前またはメールアドレス",
        "no" => "いいえ",
        "no cap" => "上限なし",
        "not done" => "未対応",
        "of cap" => "上限に対する使用率",
        "of a repo file: overrides, dirs, tracker, sonar…" => {
            "と同じ): overrides、dirs、tracker、sonar など"
        }
        "on a line of its own." => "単独の行で書いてください。",
        "out everywhere" => "をすべての場所でサインアウトします",
        "pass" => "成功",
        "person" => "人",
        "prints, in TOML." => "の出力 (TOML)。",
        "review" => "レビュー",
        "service" => "サービス",
        "set" => "設定済み",
        "shown only once" => "表示はこの 1 回だけです",
        "since" => "開始:",
        "target" => "目標",
        "target ≤ $4" => "目標 ≤ $4",
        "the cap of" => "上限:",
        "the forge" => "フォージ",
        "the fork," => "フォーク:",
        "the repo itself" => "リポジトリ本体",
        "the server's AWS role" => "サーバーの AWS ロール",
        "the value of" => "次の値:",
        "time zone. A job may spend up to" => "のタイムゾーンで行います。1 つのジョブは最大",
        "timezone. Warning threshold at" => "のタイムゾーンで行います。警告のしきい値は上限の",
        "to" => "として次に送信します:",
        "too many PRs at once" => "PR が一度に多すぎる",
        "tracked" => "記録のみ",
        "triage lane" => "トリアージレーン",
        "webhook(s) with a bad signature in the last" => "件の署名が不正な Webhook (直近",
        "whose PR was accepted is one a diagnostic lane would have missed (ADR 0012). Jobs show no triage of their own, so reviews are not swayed by it." => {
            "以外と判定されたのに PR が承認されたチケットは、診断レーンなら見落としていたものです (ADR 0012)。ジョブには自身のトリアージを表示しないため、レビューがそれに左右されることはありません。"
        }
        "yes" => "はい",
        "· Monthly budget" => "· 月間予算",
        "· change" => "· 変更",
        "· connection" => "· 接続",
        "≤ 15 min" => "≤ 15 分",
        _ => return None,
    })
}
