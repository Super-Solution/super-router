//! Values the host stores, reads and shows. Times are epoch milliseconds, amounts the host's own integer unit.

use serde::{Deserialize, Serialize};

/// The rules every referral follows; the host edits them, [`crate::Campaigns::set_settings`] checks them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// When off, nobody can attach and no rebate is made (held ones still release).
    pub referral_enabled: bool,
    /// Days a rebate waits before it is granted, so a dispute can void it. 0 grants at once.
    pub hold_days: i32,
    /// How long after an account is made an invite code may still be attached to it.
    pub attach_window_hours: i32,
    /// Rebates are paid only on purchases within this many days of the attachment; `None` for ever.
    pub rebate_window_days: Option<i32>,
    /// The most one inviter can earn in all (voided rebates do not count); `None` for no cap.
    pub rebate_cap: Option<i64>,
    /// How many of one inviter's invitees may share a signal before the next is flagged.
    pub max_per_signal: i32,
    /// Unknown codes one customer (or three times that, one network) may try in the window.
    pub code_fail_limit: i32,
    pub code_fail_window_minutes: i32,
    pub updated_at: i64,
}

/// A named share of a purchase that goes to an inviter, in basis points (300 is 3%).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct RateClass {
    pub name: String,
    pub rate_bps: i32,
    pub is_default: bool,
    pub updated_at: i64,
}

/// What a person sees of their own invitation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    pub code: String,
    pub class: String,
    pub rate_bps: i32,
    /// People attached to this code that count (active).
    pub invited: i64,
    /// Rebates granted so far.
    pub earned: i64,
    /// Rebates decided but still held.
    pub pending: i64,
}

/// What the host tells the extension about a new account that arrived through an invite code.
#[derive(Debug, Clone)]
pub struct Attach<'a> {
    pub referred: &'a str,
    pub code: &'a str,
    /// When the invited person's account was made.
    pub account_created_ms: i64,
    /// Whether they already bought anything (a referral is for new customers).
    pub has_purchased: bool,
    pub signals: &'a [crate::Signal],
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum AttachOutcome {
    /// Counted: their purchases pay the inviter.
    Attached,
    /// Recorded but earning nothing until a person clears it.
    Flagged {
        reason: String,
    },
    AlreadyAttached,
    UnknownCode,
    SelfReferral,
    /// The account is older than the window, or already bought.
    TooLate,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum PurchaseOutcome {
    /// The inviter's rebate was decided (held, or granted when the hold is 0).
    Rebate { amount: i64, held: bool },
    /// This purchase was seen before.
    Seen,
    /// Nobody to pay, or nothing to pay (no referral, flagged, outside the window, rate or amount zero).
    None,
}

/// What a new batch of codes is.
#[derive(Debug, Clone)]
pub struct NewBatch<'a> {
    pub name: &'a str,
    /// Credits each redemption gives, in the host's unit.
    pub amount: i64,
    /// Serial codes to make (1 to [`MAX_BATCH_CODES`]); ignored when `custom` is set.
    pub count: u32,
    /// A chosen code word instead of random serial codes (one code, usually with `max_uses` above 1).
    pub custom: Option<&'a str>,
    /// How many customers may use each code: 1 is single use.
    pub max_uses: i32,
    /// No redemption after this moment.
    pub redeem_until_ms: Option<i64>,
    /// The granted credits expire this many days after redemption; `None` for never.
    pub grant_days: Option<i32>,
    pub created_by: Option<&'a str>,
    pub now_ms: i64,
}

/// Most serial codes in one batch.
pub const MAX_BATCH_CODES: u32 = 5_000;

/// A batch just made: the only time the codes exist in the clear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedBatch {
    pub batch: String,
    /// Displayed form (`XXXX-XXXX-XXXX`), or the custom word.
    pub codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct BatchRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub amount: i64,
    pub max_uses: i32,
    pub redeem_until: Option<i64>,
    pub grant_days: Option<i32>,
    pub code_count: i32,
    /// Redemptions so far.
    pub redeemed: i64,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub disabled_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CodeRow {
    pub id: String,
    pub hint: String,
    pub used: i32,
    pub disabled_at: Option<i64>,
}

/// What the host tells the extension when a person enters a code.
#[derive(Debug, Clone)]
pub struct Redeem<'a> {
    pub customer: &'a str,
    pub code: &'a str,
    /// The client's address, kept only as a keyed hash to limit guessing.
    pub network: Option<&'a str>,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Redeemed {
    pub amount: i64,
    pub expires_ms: Option<i64>,
    pub batch: String,
}

/// Why a code gave nothing. Only [`RedeemError::Unknown`] counts as a guess, and not even that when the text cannot
/// be any code at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum RedeemError {
    /// No such code.
    Unknown,
    Expired,
    Exhausted,
    Disabled,
    AlreadyRedeemed,
    /// Too many unknown codes lately; try again after this moment.
    Locked {
        until_ms: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct RedemptionRow {
    pub id: String,
    pub batch: String,
    pub batch_name: String,
    pub code_hint: String,
    pub customer: String,
    pub amount: i64,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ReferrerRow {
    pub customer: String,
    pub code: String,
    pub class: String,
    pub invited: i64,
    pub flagged: i64,
    pub earned: i64,
    pub pending: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ReferralRow {
    pub referred: String,
    pub referrer: String,
    pub state: String,
    pub reason: Option<String>,
    pub attached_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct RebateRow {
    pub reference: String,
    pub referrer: String,
    pub referred: String,
    pub base: i64,
    pub rate_bps: i32,
    pub amount: i64,
    pub state: String,
    pub release_at: i64,
    pub created_at: i64,
    pub settled_at: Option<i64>,
    pub reason: Option<String>,
}

/// Totals for the program's overview.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub referrers: i64,
    pub referrals: i64,
    pub flagged: i64,
    pub rebates_held: i64,
    pub rebates_released: i64,
    pub rebates_void: i64,
    pub codes: i64,
    pub redemptions: i64,
    pub redeemed_amount: i64,
}
