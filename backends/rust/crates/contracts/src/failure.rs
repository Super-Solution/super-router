//! Why an upstream call was refused, and what that means for the next offering.
//!
//! A failed call says two different things. Some failures belong to the **request** (a malformed body, an argument
//! the model refuses): every other offering would refuse it too, so trying another is wasted and the call stops.
//! Others belong to the **offering** (its key was rejected, its model is gone, its account is out of credit, its
//! region is blocked, it is rate limited): the request is fine and another offering may serve it. Treating the second
//! kind as the first stops a request that a healthy offering was waiting to answer.
//!
//! [`classify`] reads the two things every HTTP provider gives (the status and the error's own message) and returns a [`FailureClass`]. The class names the cause, whether
//! the provider certainly did no work, and whether another offering may be tried, so a provider crate writes no
//! table of its own. Server errors (500, 502, 503, 504, 529) are deliberately **not** classified as certain: the
//! provider may have started work, so the reservation stays held.
//!
//! The message is matched against a short list of phrases per class, lower-cased, and only where the status alone
//! cannot tell (a 400 or 403 that is really a key problem, a 429 that is really an empty balance). Phrases are
//! specific on purpose: a phrase that also appears in an unrelated error would mark an offering unhealthy for the
//! wrong reason.

use crate::ProviderError;

/// What an upstream refusal means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Too many requests or tokens for now. Another offering may serve the request; this one recovers soon.
    RateLimited,
    /// The account behind the key is out of credit or over a spend cap. Recovers when someone pays.
    QuotaExhausted,
    /// The key or token was rejected. Recovers when someone fixes the credential.
    CredentialsRejected,
    /// The offering's model does not exist for this key (retired, renamed, not granted).
    ModelUnavailable,
    /// The provider refuses the location the call comes from.
    RegionBlocked,
    /// The request itself was refused (malformed, too large, unsupported). No other offering would accept it.
    RequestRejected,
    /// The provider failed, or the status is not one this table knows: it may have done work.
    Unknown,
}

impl FailureClass {
    /// A stable, lower-case name, usable as an error code or a metric label.
    pub fn name(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::QuotaExhausted => "quota_exhausted",
            Self::CredentialsRejected => "credentials_rejected",
            Self::ModelUnavailable => "model_unavailable",
            Self::RegionBlocked => "region_blocked",
            Self::RequestRejected => "request_rejected",
            Self::Unknown => "unknown",
        }
    }

    /// The class a stable [`name`](Self::name) stands for, or `None` for any other text (an error code that is not a
    /// class, such as `upstream_http_422`).
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::RateLimited,
            Self::QuotaExhausted,
            Self::CredentialsRejected,
            Self::ModelUnavailable,
            Self::RegionBlocked,
            Self::RequestRejected,
            Self::Unknown,
        ]
        .into_iter()
        .find(|c| c.name() == name)
    }

    /// Whether the failure belongs to the offering, so that another offering may be tried and this one should be
    /// avoided for a while.
    pub fn is_offering_scoped(self) -> bool {
        matches!(self, Self::RateLimited | Self::QuotaExhausted | Self::CredentialsRejected | Self::ModelUnavailable | Self::RegionBlocked)
    }

    /// The provider error for this class. `status` is the HTTP status, kept in the code of the two classes that do
    /// not name their cause. An offering-scoped failure is definitive (nothing was done, so the hold is released) and
    /// retryable; a rejected request is definitive and not retryable; anything else is unknown and keeps the hold.
    pub fn provider_error(self, status: u16) -> ProviderError {
        match self {
            Self::RateLimited | Self::QuotaExhausted | Self::CredentialsRejected | Self::ModelUnavailable | Self::RegionBlocked => {
                ProviderError::definitive(self.name(), true)
            }
            Self::RequestRejected => ProviderError::definitive(format!("upstream_http_{status}"), false),
            Self::Unknown => ProviderError::unknown(format!("upstream_http_{status}")),
        }
    }
}

// Phrases are written with spaces: the message is lower-cased and its underscores read as spaces first, so
// "API_KEY_INVALID" and "api key invalid" are one phrase.
const CREDENTIAL_PHRASES: &[&str] =
    &["api key not valid", "api key expired", "api key invalid", "invalid api key", "incorrect api key", "invalid x api key"];
const QUOTA_PHRASES: &[&str] = &[
    "insufficient quota",
    "exceeded your current quota",
    "credit balance is too low",
    "credits are depleted",
    "spend limit",
    "spending cap",
    "requires billing to be enabled",
    "billing account",
    "per day",
];
const REGION_PHRASES: &[&str] =
    &["user location is not supported", "not available in your country", "not available in your region", "not supported in your region"];
const MODEL_PHRASES: &[&str] = &["model not found", "is not found for api version", "does not exist or you do not have access"];

/// Classifies a refusal from its HTTP `status` and the error's own message (may be empty).
pub fn classify(status: u16, message: &str) -> FailureClass {
    let message = message.to_lowercase().replace(['_', '-'], " ");
    let said = |phrases: &[&str]| phrases.iter().any(|p| message.contains(p));
    if said(CREDENTIAL_PHRASES) {
        return FailureClass::CredentialsRejected;
    }
    if said(REGION_PHRASES) {
        return FailureClass::RegionBlocked;
    }
    match status {
        401 => FailureClass::CredentialsRejected,
        402 => FailureClass::QuotaExhausted,
        403 | 429 | 400 if said(QUOTA_PHRASES) => FailureClass::QuotaExhausted,
        403 => FailureClass::CredentialsRejected,
        404 => FailureClass::ModelUnavailable,
        429 => FailureClass::RateLimited,
        400 if said(MODEL_PHRASES) => FailureClass::ModelUnavailable,
        // Any other client error is the request's: 408 (a timeout the provider may have acted on) is not.
        s if (400..500).contains(&s) && s != 408 => FailureClass::RequestRejected,
        _ => FailureClass::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_decides_where_the_message_has_nothing_to_add() {
        for (status, want) in [
            (401, FailureClass::CredentialsRejected),
            (403, FailureClass::CredentialsRejected),
            (404, FailureClass::ModelUnavailable),
            (429, FailureClass::RateLimited),
            (402, FailureClass::QuotaExhausted),
            (400, FailureClass::RequestRejected),
            (413, FailureClass::RequestRejected),
            (422, FailureClass::RequestRejected),
            (409, FailureClass::RequestRejected),
            (408, FailureClass::Unknown),
            (500, FailureClass::Unknown),
            (502, FailureClass::Unknown),
            (503, FailureClass::Unknown),
            (504, FailureClass::Unknown),
            (529, FailureClass::Unknown),
            (302, FailureClass::Unknown),
        ] {
            assert_eq!(classify(status, ""), want, "{status}");
        }
    }

    #[test]
    fn a_message_can_say_a_key_problem_hides_behind_a_400_or_a_429_is_an_empty_balance() {
        assert_eq!(classify(400, "API key not valid. Please pass a valid API key."), FailureClass::CredentialsRejected);
        assert_eq!(classify(400, "API_KEY_INVALID"), FailureClass::CredentialsRejected);
        assert_eq!(classify(400, "User location is not supported for the API use."), FailureClass::RegionBlocked);
        assert_eq!(classify(400, "Your credit balance is too low to access the API"), FailureClass::QuotaExhausted);
        assert_eq!(
            classify(429, "You exceeded your current quota, please check your plan and billing details."),
            FailureClass::QuotaExhausted
        );
        assert_eq!(classify(429, "insufficient_quota"), FailureClass::QuotaExhausted);
        assert_eq!(classify(429, "Quota exceeded for metric requests per minute"), FailureClass::RateLimited);
        assert_eq!(classify(403, "Your spend limit was reached"), FailureClass::QuotaExhausted);
        assert_eq!(classify(400, "The model `x` does not exist or you do not have access to it"), FailureClass::ModelUnavailable);
        // An unrelated 400 that merely contains a common word stays the request's problem.
        assert_eq!(classify(400, "Unknown name \"foo\" at 'tools': Cannot find field."), FailureClass::RequestRejected);
        assert_eq!(classify(400, "Please use a valid role: user, model."), FailureClass::RequestRejected);
    }

    #[test]
    fn only_a_status_the_table_trusts_is_certain_and_server_errors_keep_the_hold() {
        for class in [
            FailureClass::RateLimited,
            FailureClass::QuotaExhausted,
            FailureClass::CredentialsRejected,
            FailureClass::ModelUnavailable,
            FailureClass::RegionBlocked,
        ] {
            let e = class.provider_error(400);
            assert!(e.definitive && e.retryable && e.code == class.name(), "{class:?}");
            assert!(class.is_offering_scoped());
        }
        let rejected = FailureClass::RequestRejected.provider_error(422);
        assert_eq!((rejected.definitive, rejected.retryable, rejected.code.as_str()), (true, false, "upstream_http_422"));
        let unknown = FailureClass::Unknown.provider_error(503);
        assert_eq!((unknown.definitive, unknown.retryable, unknown.code.as_str()), (false, false, "upstream_http_503"));
        assert!(!FailureClass::RequestRejected.is_offering_scoped() && !FailureClass::Unknown.is_offering_scoped());
        assert_eq!(FailureClass::from_name("quota_exhausted"), Some(FailureClass::QuotaExhausted));
        assert_eq!(FailureClass::from_name("upstream_http_422"), None);
    }
}
