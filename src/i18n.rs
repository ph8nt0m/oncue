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
    pub help: &'static str,
    pub resets: &'static str,
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
    help: "j/k move · d dormant · r refresh · q quit",
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
    help: "j/k 이동 · d 잠든 세션 · r 새로고침 · q 종료",
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
