//! The pages in Vietnamese (#590), by their English text: one arm per string
//! the templates pass to `crate::t`; a new string needs its arm here and in
//! the other catalog (`i18n::tests` checks both).

pub(super) fn get(en: &str) -> Option<&'static str> {
    Some(match en {
        "Repo memories the job was given" => "Memory của repo được đưa cho job",
        "Recalled from the repo's history and runbooks, as reference for the agent. One that no longer holds can be marked obsolete: no job is given it again until its text changes." => {
            "Lấy từ lịch sử và runbook của repo, làm tài liệu tham khảo cho agent. Memory nào không còn đúng có thể đánh dấu lỗi thời: không job nào nhận lại nó cho đến khi nội dung của nó thay đổi."
        }
        "Memory" => "Memory",
        "Source" => "Nguồn",
        "(shared with the tenant)" => "(dùng chung trong tenant)",
        "Obsolete:" => "Lỗi thời:",
        "Confirmed: the repo's next jobs are given this diagnosis as a memory." => {
            "Đã xác nhận: các job tiếp theo của repo được đưa chẩn đoán này làm memory."
        }
        "Confirm diagnosis" => "Xác nhận chẩn đoán",
        "It was right: keep it as a memory, for the next tickets like it." => {
            "Chẩn đoán đúng: giữ làm memory cho các ticket tương tự sau này."
        }
        "Why rejected (optional)" => "Lý do từ chối (không bắt buộc)",
        "With a rejected verdict, what the agent tried and your reason are kept as a pitfall: the repo's next jobs are told not to repeat it." => {
            "Khi chọn Từ chối, cách agent đã thử và lý do của bạn được giữ làm bài học cần tránh: các job tiếp theo của repo được dặn không lặp lại."
        }
        "Mark obsolete" => "Đánh dấu lỗi thời",
        "Pre-flight check" => "Kiểm tra trước khi bật",
        "Check" => "Mục kiểm tra",
        "Result" => "Kết quả",
        "No pre-flight check yet. It checks the repo's profile, clone, lockfiles, sandbox image and fetch hosts, then runs its gate on the base, as" => {
            "Chưa có lần kiểm tra nào. Lần kiểm tra xem profile, bản clone, lockfile, image sandbox và các host tải phụ thuộc của repo, rồi chạy gate trên nhánh gốc, giống như"
        }
        "does." => ".",
        "Run the pre-flight check" => "Chạy kiểm tra",
        "Turn the repo on" => "Bật repo",
        "Waiting for the first result: a check waits for a free slot, then runs the repo's gate on its base." => {
            "Đang chờ kết quả đầu tiên: lần kiểm tra chờ đến lượt, rồi chạy gate của repo trên nhánh gốc."
        }
        "blocking" => "chặn",
        "warn" => "cảnh báo",
        "skip" => "bỏ qua",
        "running" => "đang chạy",
        "passed" => "đạt",
        "warned" => "đạt, có cảnh báo",
        "failed" => "không đạt",
        "the merge request" => "merge request",
        "the pull request" => "pull request",
        "Internal server error" => "Lỗi nội bộ của server",
        "% of cap; below" => "% hạn mức; khi còn dưới",
        "(ADR 0005): acceptance target ≥ 40% overall, ≥ 35% per language. Hidden test results are not shown here." => {
            "(ADR 0005): mục tiêu tỉ lệ chấp nhận ≥ 40% tổng thể, ≥ 35% mỗi ngôn ngữ. Kết quả hidden test không hiện ở đây."
        }
        "(reserve per issue), repo accepts no new jobs; queued jobs wait for next month or until cap is raised." => {
            "(dự phòng cho mỗi issue), repo không nhận job mới; job trong hàng đợi chờ tới tháng sau hoặc tới khi hạn mức được nâng."
        }
        "(service)" => "(service)",
        ", and set it in the configuration." => {
            "bằng địa chỉ mọi người dùng để vào server này, và đặt nó trong cấu hình."
        }
        ", done" => ", xong lúc",
        ", for when this page cannot be reached. See the incident runbook." => {
            ", khi không vào được trang này. Xem runbook xử lý sự cố."
        }
        ", one per browser they signed in with." => ", mỗi trình duyệt đã đăng nhập một phiên.",
        ", or" => ", hoặc",
        ", so with less than that left under the cap no new job runs; jobs already running finish." => {
            ", nên khi phần còn lại dưới mức đó thì không job mới nào chạy; job đang chạy vẫn chạy xong."
        }
        ", then paste the token when prompted." => ", rồi dán token khi được hỏi.",
        ", which never comes through your browser. A webhook already pointing at this server is updated instead of doubled." => {
            "của chính server, giá trị này không bao giờ đi qua trình duyệt của bạn. Webhook nào đã trỏ tới server này sẽ được cập nhật thay vì tạo trùng."
        }
        ", with the roles it has in each repo." => ", với các role nó có trong từng repo.",
        ". A token does not end by itself: revoke one that is no longer used." => {
            ". Token không tự hết hạn: hãy thu hồi token không còn dùng."
        }
        ". It is used for this request only, then dropped: not kept, logged or recorded. The webhook is signed with the server's own" => {
            ". Token chỉ dùng cho yêu cầu này rồi bỏ đi: không lưu, không ghi log, không ghi nhận. Webhook được ký bằng"
        }
        ". No new job runs; queued jobs wait" => {
            ". Không job mới nào chạy; job trong hàng đợi đang chờ"
        }
        ". Queued jobs wait and new ones are recorded; none runs until the agents are resumed." => {
            ". Job trong hàng đợi sẽ chờ, job mới vẫn được ghi nhận; không job nào chạy cho tới khi agent được tiếp tục."
        }
        ". Queued jobs wait and new ones are recorded; none runs until the repo is resumed." => {
            ". Job trong hàng đợi sẽ chờ, job mới vẫn được ghi nhận; không job nào chạy cho tới khi repo được tiếp tục."
        }
        ". To have the agent work on it again, use Request changes there or comment" => {
            ". Muốn agent làm lại, hãy dùng Request changes ở đó hoặc bình luận"
        }
        ": a fork (or the repo itself) and the secret of its token, below." => {
            ": một fork (hoặc chính repo) và secret chứa token của nó, ở bên dưới."
        }
        ": copy it now." => ": hãy chép ngay.",
        ": do that on" => ": hãy làm việc đó trên",
        ": it is in paths and branch names, so it does not change." => {
            ": slug nằm trong đường dẫn và tên nhánh nên không đổi được."
        }
        ": the instance's GitHub App installed on the repo; the agent's" => {
            ": GitHub App của instance được cài trên repo; các nhánh"
        }
        "; PR is reviewed there." => "; PR được review ở đó.",
        "; this signs you out too" => "; thao tác này cũng đăng xuất chính bạn",
        "A queued job is cancelled at once; a running one stops when its worker next checks in." => {
            "Job trong hàng đợi bị huỷ ngay; job đang chạy dừng ở lần kiểm tra tiếp theo của worker."
        }
        "AMS report" => "Báo cáo AMS",
        "API tokens" => "API token",
        "Accepted" => "Được chấp nhận",
        "Accepted PRs" => "PR được chấp nhận",
        "Accepted with edits" => "Chấp nhận có chỉnh sửa",
        "Account" => "Tài khoản",
        "Action" => "Thao tác",
        "Active tokens" => "Token còn hiệu lực",
        "Add a repo" => "Thêm repo",
        "Add a service" => "Thêm service",
        "Add an admin…" => "Thêm admin…",
        "Add and clone" => "Thêm và clone",
        "Add service" => "Thêm service",
        "Admin" => "Admin",
        "Admin right" => "Quyền admin",
        "Admin ·" => "Quản trị ·",
        "Advisory" => "Advisory",
        "All agents paused" => "Mọi agent đang tạm dừng",
        "Also revoke their personal tokens, so the" => "Thu hồi luôn token cá nhân, để CLI",
        "An admin may do everything in every repo, these pages included." => {
            "Admin làm được mọi việc trong mọi repo, kể cả các trang này."
        }
        "An admin may do everything in every repo, these pages included. A person may be named before they sign in for the first time; a name without" => {
            "Admin làm được mọi việc trong mọi repo, kể cả các trang này. Có thể chỉ định một người trước lần đăng nhập đầu tiên; tên không có"
        }
        "Approve the plan" => "Duyệt kế hoạch",
        "As a repo file: what" => "Dạng file repo: những gì",
        "As an admin they may do everything in every repo, whatever their roles." => {
            "Là admin nên họ làm được mọi việc trong mọi repo, bất kể role."
        }
        "As the agent wrote it in its final answer; the gate checked the patch, not this text." => {
            "Nguyên văn agent viết trong câu trả lời cuối; gate kiểm tra bản vá, không kiểm tra đoạn này."
        }
        "Asks the forge's API, from the server, what each token may do and what the repo is called now." => {
            "Từ server, hỏi API của forge xem mỗi token làm được gì và repo hiện tên là gì."
        }
        "Attempt" => "Lần thử",
        "Attempts" => "Các lần thử",
        "Audit log" => "Nhật ký kiểm tra",
        "Average review fix rounds per accepted PR" => {
            "Số vòng sửa theo review trung bình mỗi PR được chấp nhận"
        }
        "Awaiting review" => "Chờ review",
        "Began (UTC)" => "Bắt đầu (UTC)",
        "Bias investigation flags (#511)" => "Cờ điều tra thiên lệch (#511)",
        "Bind" => "Gắn",
        "Broke tests" => "Làm hỏng test",
        "By contract and SLA level (#526). MTTR runs from the ticket coming in to the end of its job's last run, for the jobs that ended with a result. On time: of those with an SLA deadline, the ones that ended by it. Triage: resolved by a diagnosis on the ticket, with no PR." => {
            "Theo hợp đồng và mức SLA (#526). MTTR tính từ lúc ticket đến cho tới khi lần chạy cuối của job kết thúc, với các job kết thúc có kết quả. Đúng hạn: trong số có hạn SLA, những job kết thúc trước hạn. Phân loại: được giải quyết bằng chẩn đoán trên ticket, không có PR."
        }
        "By job creation week" => "Theo tuần tạo job",
        "By language" => "Theo ngôn ngữ",
        "By repo" => "Theo repo",
        "By tenant" => "Theo tenant",
        "By ticket language" => "Theo ngôn ngữ của ticket",
        "By ticket source" => "Theo nguồn ticket",
        "CI or a connector that calls the API: name it, make it a token on its page, and give it roles on the repos' pages." => {
            "CI hoặc connector gọi API: đặt tên, tạo token ở trang của nó, và cấp role ở trang của các repo."
        }
        "CLI stops working for them" => "không dùng được nữa",
        "Cache read" => "Đọc từ cache",
        "Calls" => "Số lượt gọi",
        "Cancel its queued jobs too" => "Huỷ cả các job trong hàng đợi",
        "Cancel job…" => "Huỷ job…",
        "Cap" => "Hạn mức",
        "Cap (USD / month)" => "Hạn mức (USD / tháng)",
        "Change" => "Sửa",
        "Change the cap" => "Đổi hạn mức",
        "Change this repo" => "Sửa repo này",
        "Changes" => "Thay đổi",
        "Check against" => "Kiểm tra với",
        "Check now" => "Kiểm tra ngay",
        "Check the connection" => "Kiểm tra kết nối",
        "Checked against its mode's configuration as" => {
            "Được kiểm tra với cấu hình của mode giống như"
        }
        "Clear" => "Xoá lọc",
        "Clone" => "Bản clone",
        "Clone URL" => "URL clone",
        "Connection" => "Cách kết nối",
        "Connection to" => "Kết nối tới",
        "Connection to the forge" => "Kết nối tới forge",
        "Content type" => "Content type",
        "Contract" => "Hợp đồng",
        "Conventions" => "Quy ước",
        "Shadow mode" => "Chế độ shadow",
        "jobs run to the end of the gate but push nothing, open no PR and write nothing on the ticket" => {
            "job chạy hết gate nhưng không push, không mở PR và không ghi gì lên ticket"
        }
        "Copy it now into the service's secret store; only its hash is kept here." => {
            "Hãy chép ngay vào kho bí mật của service; ở đây chỉ giữ mã băm của nó."
        }
        "Cost" => "Chi phí",
        "Cost / issue" => "Chi phí / issue",
        "Cost per contract" => "Chi phí theo hợp đồng",
        "Create CVE job" => "Tạo job CVE",
        "Create job" => "Tạo job",
        "Create token" => "Tạo token",
        "Created" => "Tạo lúc",
        "Created (UTC)" => "Tạo lúc (UTC)",
        "Created at" => "Tạo lúc",
        "Days" => "Số ngày",
        "Default branch" => "Nhánh mặc định",
        "Delivery" => "Lượt gửi",
        "Detail" => "Chi tiết",
        "Diagnosed, no PR" => "Đã chẩn đoán, không có PR",
        "Diagnosis" => "Chẩn đoán",
        "Dimension" => "Khía cạnh",
        "Done" => "Xong",
        "E-mail or service" => "E-mail hoặc service",
        "Each customer installs the app on their own org. Bind each installation to the tenant whose org it is: until then no token is minted for it, and once bound, only that tenant's repos get its tokens. A bound installation moves to another tenant only after it is unbound. The same as" => {
            "Mỗi khách hàng cài app vào org của mình. Hãy gắn mỗi lượt cài với tenant sở hữu org đó: trước khi gắn sẽ không có token nào được cấp, và sau khi gắn chỉ các repo của tenant đó nhận token. Muốn chuyển một lượt cài đã gắn sang tenant khác thì phải gỡ gắn trước. Tương đương lệnh"
        }
        "Effort" => "Mức suy luận",
        "Embedded instructions in what the job read" => "Chỉ thị nhúng trong những gì job đã đọc",
        "Embedded instructions the agent reported" => "Chỉ thị nhúng mà agent báo cáo",
        "Emergency stop" => "Dừng khẩn cấp",
        "Emergency stop: running jobs are cancelled too" => "Dừng khẩn cấp: huỷ cả job đang chạy",
        "Ended" => "Kết thúc",
        "Ends (UTC)" => "Kết thúc (UTC)",
        "Error" => "Lỗi",
        "Error · aegis" => "Lỗi · aegis",
        "Escalate to an emergency stop…" => "Nâng lên dừng khẩn cấp…",
        "Escalated" => "Đã chuyển lên",
        "Escalated to Expert" => "Chuyển lên Expert",
        "Event" => "Sự kiện",
        "Every job works on the code whatever Light read (#497). A ticket read as other than" => {
            "Mọi job vẫn làm việc với code bất kể Light đọc ra gì (#497). Một ticket được đọc khác với"
        }
        "Every registered repo. Open one to see its settings and clone, change it, and change who has which role in it." => {
            "Mọi repo đã đăng ký. Mở một repo để xem cài đặt và bản clone, sửa nó, và đổi ai có role gì trong đó."
        }
        "Every running job is asked to cancel; each stops at its worker's next lease renewal." => {
            "Mọi job đang chạy sẽ được yêu cầu huỷ; mỗi job dừng ở lần gia hạn lease tiếp theo của worker."
        }
        "Every write, by whom and when: roles, repos, jobs, reviews, tokens, budgets. Newest first; it cannot be changed." => {
            "Mọi thao tác ghi, ai làm và khi nào: role, repo, job, review, token, ngân sách. Mới nhất trước; không thể sửa."
        }
        "Existing review:" => "Review đã có:",
        "Expires" => "Hết hạn",
        "Filter" => "Lọc",
        "Find" => "Tìm",
        "Flags" => "Cờ",
        "Flags appear once a slice reaches 5 finished attempts (ADR 0016)." => {
            "Cờ hiện khi một nhóm có đủ 5 lần chạy đã xong (ADR 0016)."
        }
        "For" => "Dùng cho",
        "For a lost device, an account at risk, or someone leaving. They can sign in again unless the SSO stops them" => {
            "Dùng khi mất thiết bị, tài khoản có nguy cơ bị lộ, hoặc có người nghỉ việc. Họ vẫn đăng nhập lại được trừ khi SSO chặn"
        }
        "For a token that leaked, or a service no longer used. What uses them stops at once." => {
            "Dùng khi token bị lộ, hoặc service không còn dùng. Những gì đang dùng các token này sẽ dừng ngay."
        }
        "For an incident that is not one repo's: a model acting strangely, a prompt injection spreading, a provider billing wrong. Webhooks, trackers and Sonar still record jobs while paused; nothing in the queue is lost. To hold one repo, pause it from its own page." => {
            "Dành cho sự cố không thuộc riêng một repo: model hoạt động bất thường, prompt injection lan rộng, nhà cung cấp tính phí sai. Trong lúc tạm dừng, webhook, tracker và Sonar vẫn ghi nhận job; không mất gì trong hàng đợi. Muốn giữ riêng một repo, hãy tạm dừng nó từ trang của repo đó."
        }
        "For review rounds on the agent's pull requests." => {
            "Dùng cho các vòng review trên pull request của agent."
        }
        "Fork" => "Fork",
        "Found by the platform, not reported by the agent. The repo or the issue may be targeted: check the source." => {
            "Do platform phát hiện, không phải agent báo cáo. Repo hoặc issue có thể đang bị nhắm tới: hãy kiểm tra nguồn."
        }
        "From" => "Nguồn",
        "GitHub App" => "GitHub App",
        "GitHub App installations" => "Các lượt cài GitHub App",
        "Give a token that may manage webhooks there:" => {
            "Nhập một token có quyền quản lý webhook ở đó:"
        }
        "Give role" => "Cấp role",
        "Group" => "Nhóm",
        "Guard:" => "Kiểm soát:",
        "Held: no verifiable sources" => "Giữ lại: không có nguồn kiểm chứng được",
        "How far" => "Mức độ",
        "How the platform reaches the repo" => "Cách platform truy cập repo",
        "In US dollars a month;" => "Tính bằng USD mỗi tháng; đến nay đã chi",
        "In the store" => "Trong kho",
        "Installation" => "Lượt cài",
        "Installed, not registered" => "Đã cài, chưa đăng ký",
        "Issue" => "Issue",
        "Job" => "Job",
        "Job #" => "Job #",
        "Job budget" => "Ngân sách mỗi job",
        "Jobs" => "Job",
        "Jobs at once" => "Số job chạy cùng lúc",
        "Key" => "Khoá",
        "Kind" => "Loại",
        "Label" => "Nhãn",
        "Label (machine, usage)" => "Nhãn (máy, mục đích)",
        "Language" => "Ngôn ngữ",
        "Last seen (UTC)" => "Lần cuối thấy (UTC)",
        "Last webhooks" => "Webhook gần nhất",
        "Ledger" => "Sổ cái",
        "Ledger history" => "Lịch sử sổ cái",
        "Ledger · all completed runs" => "Sổ cái · mọi lần chạy đã xong",
        "Ledger · jobs created this month" => "Sổ cái · job tạo trong tháng này",
        "Less than" => "Còn dưới",
        "Level" => "Mức",
        "Light read the ticket as" => "Light đọc ticket là",
        "Lines edited" => "Số dòng đã sửa",
        "MTTR and SLA" => "MTTR và SLA",
        "Made (UTC)" => "Tạo lúc (UTC)",
        "Main" => "Chính",
        "Maintainers create, cancel and retry jobs; reviewers also review PRs; viewers read. A person is named by the e-mail the SSO gives, and may be given a role before signing in for the first time; a name without" => {
            "Maintainer tạo, huỷ và chạy lại job; reviewer còn review PR; viewer chỉ xem. Một người được gọi bằng e-mail mà SSO trả về, và có thể được cấp role trước lần đăng nhập đầu tiên; tên không có"
        }
        "Make a token" => "Tạo token",
        "Make admin" => "Cấp quyền admin",
        "Make admin…" => "Cấp quyền admin…",
        "Make someone an admin" => "Cấp quyền admin cho một người",
        "Make the webhook" => "Tạo webhook",
        "Make them by hand on the forge, or let an admin make them from here with a token given once: set" => {
            "Hãy tạo thủ công trên forge, hoặc để admin tạo từ đây bằng một token dùng một lần: đặt"
        }
        "Make them from here" => "Tạo từ đây",
        "Mean cost" => "Chi phí trung bình",
        "Mode" => "Mode",
        "Model" => "Model",
        "Model calls" => "Lượt gọi model",
        "Model tiers" => "Các tầng model",
        "Month tracked in" => "Tháng được tính theo múi giờ",
        "Monthly budget" => "Ngân sách tháng",
        "Monthly cap" => "Hạn mức tháng",
        "My tokens" => "Token của tôi",
        "Name" => "Tên",
        "New job" => "Job mới",
        "New token," => "Token mới,",
        "Next page →" => "Trang sau →",
        "No PR" => "Không có PR",
        "No attempts yet." => "Chưa có lần thử nào.",
        "No clone made from these pages; one made with" => {
            "Chưa có bản clone nào tạo từ các trang này; bản clone tạo bằng"
        }
        "No events in ledger yet." => "Sổ cái chưa có sự kiện nào.",
        "No installation recorded yet: they arrive with the app's" => {
            "Chưa có lượt cài nào được ghi nhận: chúng đến cùng sự kiện"
        }
        "No job created this month yet." => "Tháng này chưa có job nào được tạo.",
        "No jobs match the filter." => "Không job nào khớp bộ lọc.",
        "No live session: they are signed out everywhere." => {
            "Không có phiên nào còn hiệu lực: người này đã đăng xuất ở mọi nơi."
        }
        "No live token." => "Không có token nào còn hiệu lực.",
        "No one" => "Không có ai",
        "No one has a role in this repo yet." => "Chưa ai có role trong repo này.",
        "No repo registered yet." => "Chưa có repo nào được đăng ký.",
        "No repos yet." => "Chưa có repo nào.",
        "No role in any repo. Give one on the repo's page." => {
            "Không có role trong repo nào. Hãy cấp role ở trang của repo."
        }
        "No runs yet." => "Chưa có lần chạy nào.",
        "No tokens yet." => "Chưa có token nào.",
        "No webhook from this repo yet. GitHub sends a ping when the webhook is made; GitLab's \"Test\" sends a sample event." => {
            "Chưa có webhook nào từ repo này. GitHub gửi một ping khi tạo webhook; nút \"Test\" của GitLab gửi một sự kiện mẫu."
        }
        "None of its own" => "Không có hạn mức riêng",
        "Not configured: the server's" => "Chưa cấu hình:",
        "Note" => "Ghi chú",
        "Nothing" => "Không có gì",
        "Older →" => "Cũ hơn →",
        "On" => "Đối tượng",
        "On the fork" => "Trên fork",
        "On time" => "Đúng hạn",
        "Open PR" => "Mở PR",
        "Open PR in new tab" => "Mở PR trong tab mới",
        "Open, overdue" => "Chưa xong, quá hạn",
        "Other settings" => "Cài đặt khác",
        "Outcome" => "Kết quả",
        "Owner" => "Chủ sở hữu",
        "PR broke existing tests" => "PR làm hỏng test có sẵn",
        "PRs opened" => "PR đã mở",
        "PRs without review" => "PR chưa được review",
        "Package" => "Package",
        "Pause all" => "Tạm dừng tất cả",
        "Pause all agents" => "Tạm dừng mọi agent",
        "Pause all agents…" => "Tạm dừng mọi agent…",
        "Pause or resume" => "Tạm dừng hoặc tiếp tục",
        "Pause the repo" => "Tạm dừng repo",
        "Pause the repo when it runs wrong or spends too fast: its queued jobs wait, and new ones are still recorded, until it is resumed. Jobs already running finish; cancel one from its own page." => {
            "Tạm dừng repo khi nó chạy sai hoặc tiêu tiền quá nhanh: job trong hàng đợi sẽ chờ, job mới vẫn được ghi nhận, cho tới khi repo được tiếp tục. Job đang chạy vẫn chạy xong; muốn huỷ thì huỷ từ trang của từng job."
        }
        "Paused" => "Đang tạm dừng",
        "Payload URL" => "Payload URL",
        "People (by the e-mail the SSO gives) and services (API tokens). Open one to see their roles and change the admin right; roles in a repo are given on the repo's page." => {
            "Người (theo e-mail SSO trả về) và service (API token). Mở một mục để xem role và đổi quyền admin; role trong repo được cấp ở trang của repo."
        }
        "Platform" => "Nền tảng",
        "Provider" => "Nhà cung cấp",
        "Raise a dependency for an advisory" => "Nâng một dependency theo advisory",
        "Received (UTC)" => "Nhận lúc (UTC)",
        "Recent webhooks" => "Webhook gần đây",
        "Rejected" => "Bị từ chối",
        "Remaining" => "Còn lại",
        "Remove the cap" => "Bỏ hạn mức",
        "Replace existing review" => "Thay review đã có",
        "Replies to the review" => "Trả lời review",
        "Repo" => "Repo",
        "Repos" => "Repo",
        "Reproduction test:" => "Test tái hiện:",
        "Resume" => "Tiếp tục",
        "Resume all agents…" => "Tiếp tục mọi agent…",
        "Resume the repo" => "Tiếp tục repo",
        "Retry as a new job" => "Chạy lại thành job mới",
        "Review PR" => "Review PR",
        "Review fix round for PR opened by" => "Vòng sửa theo review cho PR mở bởi",
        "Review fix rounds" => "Các vòng sửa theo review",
        "Review job #" => "Review job #",
        "Review rounds / PR" => "Vòng review / PR",
        "Review time" => "Thời gian review",
        "Review time / PR" => "Thời gian review / PR",
        "Review time is calculated automatically from opening this page until submission." => {
            "Thời gian review được tự tính từ lúc mở trang này tới lúc gửi."
        }
        "Revoke" => "Thu hồi",
        "Revoke every token…" => "Thu hồi mọi token…",
        "Role" => "Role",
        "Roles" => "Role",
        "Round" => "Vòng",
        "Running" => "Đang chạy",
        "Runs" => "Số lần chạy",
        "Runs again" => "Chạy lại",
        "Same formulas as" => "Cùng công thức với",
        "Save and fetch" => "Lưu và tải về",
        "Save cap" => "Lưu hạn mức",
        "Secret" => "Secret",
        "Secrets" => "Secret",
        "Sends one short request to the light model; it costs a little, so once a minute at most." => {
            "Gửi một yêu cầu ngắn tới model light; việc này tốn một ít chi phí nên tối đa mỗi phút một lần."
        }
        "Sessions" => "Phiên đăng nhập",
        "Settings" => "Cài đặt",
        "Show" => "Xem",
        "Sign in" => "Đăng nhập",
        "Sign in with SSO" => "Đăng nhập bằng SSO",
        "Sign in · aegis" => "Đăng nhập · aegis",
        "Sign in:" => "Đăng nhập:",
        "Sign out" => "Đăng xuất",
        "Sign out everywhere…" => "Đăng xuất ở mọi nơi…",
        "Since (UTC)" => "Từ lúc (UTC)",
        "Slug" => "Slug",
        "Spent" => "Đã chi",
        "Spent this month" => "Đã chi tháng này",
        "Started" => "Bắt đầu",
        "State" => "Trạng thái",
        "Status" => "Trạng thái",
        "Stop taking jobs: running jobs finish" => "Ngừng nhận job: job đang chạy vẫn chạy xong",
        "Stopped" => "Dừng",
        "Stopped without PR" => "Dừng mà không có PR",
        "Submit review" => "Gửi review",
        "TOML, as" => "TOML, như mục",
        "Take away" => "Thu hồi",
        "Take the admin right…" => "Gỡ quyền admin…",
        "Tenant" => "Tenant",
        "Termination reason:" => "Lý do kết thúc:",
        "The GitHub App is installed on these repos; registering one still takes its language, mode and limits." => {
            "GitHub App đã được cài trên các repo này; muốn đăng ký vẫn cần chọn ngôn ngữ, mode và giới hạn."
        }
        "The end of git's output:" => "Phần cuối output của git:",
        "The model provider of each mode, as the server loaded it from the mode's file at start. Keys stay in the secret store: only their names, and whether the store has them, show here. Changing a provider or a model is a change to the mode's file and a restart." => {
            "Nhà cung cấp model của từng mode, như server đã đọc từ file của mode lúc khởi động. Khoá vẫn nằm trong kho bí mật: ở đây chỉ hiện tên khoá và kho có khoá đó hay không. Muốn đổi nhà cung cấp hay model thì sửa file của mode rồi khởi động lại."
        }
        "The month is counted in the" => "Tháng được tính theo múi giờ",
        "The name the token has in the secret store, never the token itself." => {
            "Tên của token trong kho bí mật, không bao giờ là chính token."
        }
        "The new token, shown this once." => "Token mới, chỉ hiện lần này.",
        "The patch was truncated here; the PR has it whole." => {
            "Bản vá bị cắt bớt ở đây; PR có đầy đủ."
        }
        "The platform raises the package with the repo's upgrade command; the agent only adapts the code if the checks fail (ADR 0015)." => {
            "Platform nâng package bằng lệnh nâng cấp của repo; agent chỉ sửa code khi các kiểm tra thất bại (ADR 0015)."
        }
        "The same as" => "Tương đương",
        "The server's" => "Mục",
        "The service sends it as" => "Service gửi token dưới dạng",
        "They keep their roles in each repo. The last admin cannot lose the right." => {
            "Họ vẫn giữ role trong từng repo. Admin cuối cùng không thể mất quyền."
        }
        "This is you. Another admin can take your admin right; the last admin keeps it." => {
            "Đây là bạn. Admin khác có thể gỡ quyền admin của bạn; admin cuối cùng thì luôn giữ quyền."
        }
        "This repo's audit log" => "Nhật ký kiểm tra của repo này",
        "This server has not loaded the modes' configuration: add repos with" => {
            "Server này chưa tải cấu hình của các mode: hãy thêm repo bằng"
        }
        "Tier" => "Tầng",
        "Token" => "Token",
        "Token, used once" => "Token, chỉ dùng một lần",
        "Token: name of the secret" => "Token: tên của secret",
        "Tokens" => "Token",
        "Tokens for the" => "Token cho client",
        "Total PR" => "Tổng của PR",
        "Triage" => "Phân loại",
        "Triage miss" => "Phân loại sót",
        "Triage, recorded only" => "Phân loại, chỉ ghi nhận",
        "UTC. They name no repo; if they are yours, the webhook's secret is not the value of" => {
            "UTC. Chúng không gắn với repo nào; nếu là của bạn thì secret của webhook không khớp với giá trị của"
        }
        "Unable to load page" => "Không tải được trang",
        "Unbind" => "Gỡ gắn",
        "Usage" => "Mức dùng",
        "Users" => "Người dùng",
        "Verdict" => "Đánh giá",
        "Version" => "Phiên bản",
        "Wall time" => "Thời gian thực",
        "Webhooks to make" => "Webhook cần tạo",
        "What became of the forge's last" => "Kết quả của",
        "What the agent did" => "Những gì agent đã làm",
        "What they did, in the audit log" => "Những gì họ đã làm, trong nhật ký kiểm tra",
        "When (UTC)" => "Thời điểm (UTC)",
        "Where" => "Ở đâu",
        "Where the agent pushes and opens pull requests, on the" => {
            "Nơi agent push và mở pull request, với cách kết nối"
        }
        "Whether the forge can reach this server and the tokens can do their part, without labelling a real issue to try." => {
            "Kiểm tra forge có tới được server này và các token có làm được phần việc của mình không, mà không cần gắn nhãn một issue thật để thử."
        }
        "Whether the secret store has each one; their values are never shown." => {
            "Kho bí mật có từng secret hay không; giá trị không bao giờ được hiển thị."
        }
        "Who" => "Ai",
        "Why (optional)" => "Lý do (không bắt buộc)",
        "Why, and what to do" => "Lý do và cách xử lý",
        "Workers claim the queued jobs again at once, oldest first, within each repo's limits. Check the cause is fixed first." => {
            "Worker sẽ nhận lại ngay các job trong hàng đợi, cũ nhất trước, trong giới hạn của từng repo. Hãy kiểm tra nguyên nhân đã được khắc phục trước."
        }
        "Written back to the ticket instead of a pull request (#451). Read-only: aegis changed nothing; each finding was checked against the source it cites." => {
            "Được ghi lại vào ticket thay cho pull request (#451). Chỉ đọc: aegis không thay đổi gì; mỗi phát hiện đều đã được đối chiếu với nguồn nó trích dẫn."
        }
        "Yes, cancel job #" => "Đồng ý, huỷ job #",
        "Yes, make" => "Đồng ý, cấp cho",
        "Yes, pause all agents" => "Đồng ý, tạm dừng mọi agent",
        "Yes, resume all agents" => "Đồng ý, tiếp tục mọi agent",
        "Yes, revoke every token of" => "Đồng ý, thu hồi mọi token của",
        "Yes, sign" => "Đồng ý, đăng xuất",
        "Yes, stop every running job" => "Đồng ý, dừng mọi job đang chạy",
        "Yes, take it from" => "Đồng ý, gỡ quyền của",
        "Your verdict is recorded for the pilot's measures: acceptance, the lines you edited and your review time. It does not approve, merge or change the" => {
            "Đánh giá của bạn được ghi lại cho các chỉ số của đợt thử nghiệm: chấp nhận hay không, số dòng bạn đã sửa và thời gian review. Nó không phê duyệt, merge hay thay đổi"
        }
        "active tokens, valid for up to" => "token còn hiệu lực, mỗi token dùng được tối đa",
        "added to the agent's instructions" => "được thêm vào chỉ dẫn cho agent",
        "admin" => "admin",
        "all" => "tất cả",
        "an admin" => "quyền admin",
        "and" => "và",
        "and running ones were asked to cancel" => "và các job đang chạy đã được yêu cầu huỷ",
        "any" => "tất cả",
        "at" => "lúc",
        "branches and PRs go to the repo itself and the fields below stay empty." => {
            "và PR của agent đi thẳng vào repo, các ô bên dưới để trống."
        }
        "budget" => "ngân sách",
        "by" => "bởi",
        "checks it. Its clone is made after, in the background; the repo's page shows how it goes." => {
            "kiểm tra. Bản clone được tạo sau đó, chạy nền; trang của repo cho biết tiến độ."
        }
        "choose…" => "chọn…",
        "client on your machine: acts as you, with your exact permissions on each repo, recorded under your name in audit logs. Up to" => {
            "trên máy của bạn: hành động thay bạn, với đúng quyền của bạn trên từng repo, và được ghi tên bạn trong nhật ký kiểm tra. Tối đa"
        }
        "connection page" => "trang kết nối",
        "connection." => ".",
        "days each." => "ngày.",
        "deliveries for this repo, kept 30 days: the job each made or found, or why it made none. Deliveries with a bad signature name no repo; see the" => {
            "lượt gửi gần nhất từ forge cho repo này, lưu 30 ngày: job mà mỗi lượt tạo ra hoặc tìm thấy, hoặc lý do không tạo job. Lượt gửi có chữ ký sai không gắn với repo nào; xem"
        }
        "does not show here." => "không hiện ở đây.",
        "done" => "đã xong",
        "events." => "của app.",
        "fail" => "lỗi",
        "files, as the gate checked them." => "file, đúng như gate đã kiểm tra.",
        "for these filters" => "với bộ lọc này",
        "has no file for it, so no repo can use it." => {
            "của server không có file cho mode này, nên chưa repo nào dùng được."
        }
        "hours, the last at" => "giờ qua, lần cuối lúc",
        "in the server's configuration." => "trong cấu hình của server.",
        "investigate > 30%" => "điều tra nếu > 30%",
        "is a service." => "là một service.",
        "is left under this cap, so no new job of this repo runs: queued jobs wait until next month, or until the cap is raised." => {
            "trong hạn mức này nên không job mới nào của repo được chạy: job trong hàng đợi chờ tới tháng sau, hoặc tới khi hạn mức được nâng."
        }
        "is not set: put the address people reach this server at in place of" => {
            "của server chưa được đặt: hãy thay"
        }
        "is spent so far. Leave it empty for no cap of the repo's own, so that only each job's cap and the mode's budget apply." => {
            ". Để trống nếu repo không có hạn mức riêng, khi đó chỉ áp dụng hạn mức của từng job và ngân sách của mode."
        }
        "job" => "job",
        "job #" => "job #",
        "live session" => "phiên đang hoạt động",
        "live sessions" => "phiên đang hoạt động",
        "live token" => "token còn hiệu lực",
        "live tokens" => "token còn hiệu lực",
        "matches" => "khớp",
        "me" => "tôi",
        "missing" => "thiếu",
        "model acts strangely" => "model hoạt động bất thường",
        "name or e-mail" => "tên hoặc e-mail",
        "no" => "không",
        "no cap" => "không giới hạn",
        "not done" => "chưa xong",
        "of cap" => "so với hạn mức",
        "of a repo file: overrides, dirs, tracker, sonar…" => {
            "của file repo: overrides, dirs, tracker, sonar…"
        }
        "on a line of its own." => "trên một dòng riêng.",
        "out everywhere" => "ở mọi nơi",
        "pass" => "đạt",
        "person" => "người",
        "prints, in TOML." => "in ra, dạng TOML.",
        "review" => "review",
        "service" => "service",
        "set" => "đã có",
        "shown only once" => "chỉ hiện một lần",
        "since" => "bắt đầu",
        "target" => "mục tiêu",
        "target ≤ $4" => "mục tiêu ≤ $4",
        "the cap of" => "hạn mức của",
        "the forge" => "forge",
        "the fork," => "fork,",
        "the repo itself" => "chính repo",
        "the server's AWS role" => "AWS role của server",
        "the value of" => "giá trị của",
        "time zone. A job may spend up to" => ". Một job có thể tiêu tới",
        "timezone. Warning threshold at" => ". Ngưỡng cảnh báo ở",
        "to" => "tới",
        "too many PRs at once" => "quá nhiều PR cùng lúc",
        "tracked" => "theo dõi",
        "triage lane" => "luồng phân loại",
        "webhook(s) with a bad signature in the last" => "webhook có chữ ký sai trong",
        "whose PR was accepted is one a diagnostic lane would have missed (ADR 0012). Jobs show no triage of their own, so reviews are not swayed by it." => {
            "mà PR vẫn được chấp nhận là ticket luồng chẩn đoán sẽ bỏ sót (ADR 0012). Job không hiện kết quả phân loại của mình, nên review không bị ảnh hưởng."
        }
        "yes" => "có",
        "· Monthly budget" => "· Ngân sách tháng",
        "· change" => "· sửa",
        "· connection" => "· kết nối",
        "≤ 15 min" => "≤ 15 phút",
        _ => return None,
    })
}
