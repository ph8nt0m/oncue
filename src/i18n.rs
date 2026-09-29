use crate::model::{Attention, PrState, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Ko,
}

impl Lang {
    pub fn detect(configured: &str) -> Self {
        let pick = |v: &str| {
            if v.to_ascii_lowercase().starts_with("ko") {
                Lang::Ko
            } else {
                Lang::En
            }
        };
        if !configured.is_empty() {
            return pick(configured);
        }
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .map(|v| pick(&v))
            .unwrap_or(Lang::En)
    }
}

/// Every user-facing string, one field per language.
pub struct Text {
    pub needs_you: &'static str,
    pub working: &'static str,
    pub dormant: &'static str,
    pub detail: &'static str,
    pub nothing_waiting: &'static str,
    pub loading: &'static str,
    pub options: &'static str,
    pub prs: &'static str,
    pub issues: &'static str,
    pub overlaps: &'static str,
    pub help: &'static str,
    pub resets: &'static str,
    pub confirm_reply: &'static str,
    pub confirm_allow: &'static str,
    pub confirm_keys: &'static str,
    pub sent: &'static str,
    pub failed: &'static str,
    pub opened: &'static str,
    pub nothing_to_open: &'static str,
    pub not_paseo: &'static str,
    pub no_permit: &'static str,
    pub reset_in: &'static str,
    pub reset_passed: &'static str,
}

const EN: Text = Text {
    needs_you: "Needs you",
    working: "Working",
    dormant: "Dormant",
    detail: "Detail",
    nothing_waiting: "Nothing is waiting on you.",
    loading: "Loading…",
    options: "Options",
    prs: "PRs",
    issues: "Issues",
    overlaps: "Same issue or branch as",
    help: "j/k move · ⏎ open · 1-9 reply · a allow · d dormant · r refresh · q quit",
    confirm_reply: "Send",
    confirm_allow: "Allow",
    confirm_keys: "y send · n cancel",
    sent: "Done",
    failed: "Failed",
    opened: "Opened",
    nothing_to_open: "Nothing to open for this row",
    not_paseo: "Replies and approvals work on Paseo agents only",
    no_permit: "No pending permission request",
    resets: "Limit resets",
    reset_in: "in",
    reset_passed: "reset passed, ready to resume",
};

const KO: Text = Text {
    needs_you: "나를 기다림",
    working: "작업 중",
    dormant: "잠듦",
    detail: "상세",
    nothing_waiting: "기다리는 세션이 없습니다.",
    loading: "불러오는 중…",
    options: "선택지",
    prs: "PR",
    issues: "이슈",
    overlaps: "같은 이슈·브랜치를 잡은 세션",
    help: "j/k 이동 · ⏎ 열기 · 1-9 답장 · a 허용 · d 잠든 세션 · r 새로고침 · q 종료",
    confirm_reply: "보내기",
    confirm_allow: "허용",
    confirm_keys: "y 실행 · n 취소",
    sent: "완료",
    failed: "실패",
    opened: "열었습니다",
    nothing_to_open: "열 대상이 없습니다",
    not_paseo: "답장·허용은 paseo 에이전트에만 됩니다",
    no_permit: "대기 중인 권한 요청이 없습니다",
    resets: "한도 초기화",
    reset_in: "남은 시간",
    reset_passed: "초기화됨, 이어서 진행 가능",
};

impl Lang {
    pub fn text(self) -> &'static Text {
        match self {
            Lang::En => &EN,
            Lang::Ko => &KO,
        }
    }

    pub fn state(self, state: &State) -> &'static str {
        match (self, state) {
            (Lang::En, State::Working) => "working",
            (Lang::Ko, State::Working) => "작업 중",
            (Lang::En, State::Dormant) => "dormant",
            (Lang::Ko, State::Dormant) => "잠듦",
            (lang, State::NeedsYou(a)) => lang.attention(*a),
        }
    }

    pub fn attention(self, a: Attention) -> &'static str {
        match (self, a) {
            (Lang::En, Attention::Question) => "question",
            (Lang::En, Attention::Permission) => "permit",
            (Lang::En, Attention::Plan) => "plan",
            (Lang::En, Attention::Merge) => "merge",
            (Lang::En, Attention::Error) => "error",
            (Lang::En, Attention::Blocked) => "blocked",
            (Lang::En, Attention::Limited) => "limit",
            (Lang::En, Attention::Unread) => "done",
            (Lang::En, Attention::Idle) => "idle",
            (Lang::Ko, Attention::Question) => "질문",
            (Lang::Ko, Attention::Permission) => "권한",
            (Lang::Ko, Attention::Plan) => "계획",
            (Lang::Ko, Attention::Merge) => "머지",
            (Lang::Ko, Attention::Error) => "오류",
            (Lang::Ko, Attention::Blocked) => "막힘",
            (Lang::Ko, Attention::Limited) => "한도",
            (Lang::Ko, Attention::Unread) => "보고",
            (Lang::Ko, Attention::Idle) => "대기",
        }
    }
}

impl Lang {
    pub fn pr_state(self, s: PrState) -> &'static str {
        let ko = self == Lang::Ko;
        match s {
            PrState::Merged => {
                if ko {
                    "머지됨"
                } else {
                    "merged"
                }
            }
            PrState::Closed => {
                if ko {
                    "닫힘"
                } else {
                    "closed"
                }
            }
            PrState::Draft => {
                if ko {
                    "초안"
                } else {
                    "draft"
                }
            }
            PrState::Conflict => {
                if ko {
                    "충돌"
                } else {
                    "conflict"
                }
            }
            PrState::ChecksFailed => {
                if ko {
                    "CI 실패"
                } else {
                    "checks failed"
                }
            }
            PrState::ChangesRequested => {
                if ko {
                    "변경 요청"
                } else {
                    "changes requested"
                }
            }
            PrState::Unresolved => {
                if ko {
                    "미해결 스레드"
                } else {
                    "unresolved threads"
                }
            }
            PrState::Pending => {
                if ko {
                    "CI 진행 중"
                } else {
                    "checks running"
                }
            }
            PrState::Behind => {
                if ko {
                    "베이스 뒤처짐"
                } else {
                    "behind base"
                }
            }
            PrState::Ready => {
                if ko {
                    "머지 가능"
                } else {
                    "ready to merge"
                }
            }
        }
    }
}

/// Compact age: `42s`, `7m`, `3h`, `2d`.
pub fn age(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}
