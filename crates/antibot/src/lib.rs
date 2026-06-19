pub mod cdp_stealth;
pub mod challenge_detect;
pub mod cookie_jar;
pub mod http_stealth;

pub use cdp_stealth::stealth_script;
pub use challenge_detect::{detect_challenge, detect_empty_or_blocked};
pub use cookie_jar::CookieJar;
pub use http_stealth::{
    BrowserProfile, DelayPreset, RequestDelay, StealthHeaders, UserAgentRotator, BROWSER_PROFILES,
    USER_AGENTS,
};
