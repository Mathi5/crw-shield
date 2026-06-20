pub mod block_detection;
pub mod cdp_stealth;
pub mod challenge_detect;
pub mod cookie_jar;
pub mod firefox_profiles;
pub mod http_stealth;
pub mod rotation;
pub mod situation;

pub use cdp_stealth::stealth_script;
pub use challenge_detect::{detect_challenge, detect_empty_or_blocked};
pub use cookie_jar::CookieJar;
pub use http_stealth::{
    BrowserProfile, DelayPreset, RequestDelay, StealthHeaders, UserAgentRotator, BROWSER_PROFILES,
    USER_AGENTS,
};
pub use firefox_profiles::{default_firefox_profiles, FirefoxProfile};
pub use rotation::{
    decide as decide_rotation, RotationDecision, MAX_ROTATIONS_PER_HOST,
    L2_COOLDOWN,
};
pub use block_detection::{
    counter_for as counter_for_host, detect as detect_block, BlockKind, BlockSignal,
    HostBlockCounter, HostCounters, BLOCK_THRESHOLD, EMPTY_THRESHOLD_BYTES,
};
pub use situation::{
    diagnose as diagnose_situation, Evidence, EvidenceKind, SituationKind, SituationReport,
    SuggestedLadder,
};
