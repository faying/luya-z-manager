//! What the forwarder keeps on disk: the user's settings and rules (`sms_forward.json`) and the
//! per-delivery log lines in the state file. Key names and `type` tags are the file and API
//! format, so they never change.

use serde::{Deserialize, Serialize};

/// Settings plus rules, as saved and as `/api/sms/forward/config` shows them.
#[derive(Clone, Deserialize, Serialize)]
pub struct SmsForwardConfig {
    /// Master switch.
    pub enabled: bool,
    /// Kept for the UI and older files; the loop is event driven.
    #[serde(default = "default_poll")]
    pub poll_interval_secs: u64,
    /// After every rule succeeded: mark the SMS read on the device.
    #[serde(default)]
    pub mark_read_after_forward: bool,
    /// After every rule succeeded: delete it from the device.
    #[serde(default)]
    pub delete_after_forward: bool,
    /// Tried in order for every new SMS.
    #[serde(default)]
    pub rules: Vec<ForwardRule>,
}

const DEFAULT_POLL_SECS: u64 = 30;

fn default_poll() -> u64 {
    DEFAULT_POLL_SECS
}

/// Off, no rules, nothing done to the device's copy.
impl Default for SmsForwardConfig {
    fn default() -> SmsForwardConfig {
        let off = false;
        SmsForwardConfig {
            poll_interval_secs: DEFAULT_POLL_SECS,
            enabled: off,
            delete_after_forward: off,
            rules: vec![],
            mark_read_after_forward: off,
        }
    }
}

/// Which SMS (filter) go where (destination).
#[derive(Clone, Deserialize, Serialize)]
pub struct ForwardRule {
    /// Handed out on create, never reused while the rule exists.
    pub id: u32,
    /// Shown in the UI and the log.
    pub name: String,
    /// Off = kept but skipped.
    pub enabled: bool,
    /// Which SMS match.
    pub filter: SmsFilter,
    /// Where a match is sent.
    pub destination: ForwardDestination,
}

/// Match on the sender (`*` at one end = prefix/suffix), on words in the text, both, or nothing.
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SmsFilter {
    /// every SMS
    All,
    /// sender patterns only
    Sender {
        /// `+86*`, `*1234` or an exact sender
        patterns: Vec<String>,
    },
    /// keywords only (case-insensitive substring)
    Content {
        /// any one of these words
        keywords: Vec<String>,
    },
    /// a sender pattern and a keyword must both hit
    SenderAndContent {
        /// as in `sender`
        patterns: Vec<String>,
        /// as in `content`
        keywords: Vec<String>,
    },
}

/// The places an SMS can be sent on to.
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ForwardDestination {
    /// Bot API `sendMessage`.
    Telegram {
        /// numeric chat or `@channel`
        chat_id: String,
        /// from @BotFather
        bot_token: String,
        /// no notification sound
        #[serde(default)]
        silent: bool,
    },
    /// A JSON body to any URL.
    Webhook {
        url: String,
        /// `PUT`, anything else is sent as `POST`
        #[serde(default = "post")]
        method: String,
        /// extra request headers
        #[serde(default)]
        headers: Vec<HttpHeader>,
    },
    /// Another phone, as an SMS from this device.
    Sms {
        /// the phone that receives the copy
        forward_number: String,
    },
    /// ntfy server + topic, optional bearer token.
    Ntfy {
        /// topic name, appended to the server URL
        topic: String,
        /// access token for a protected topic
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        /// server base URL
        url: String,
    },
    /// Discord channel webhook.
    Discord {
        /// channel webhook URL
        webhook_url: String,
    },
    /// Slack incoming webhook.
    Slack {
        /// incoming-webhook URL
        webhook_url: String,
    },
}

fn post() -> String {
    String::from("POST")
}

/// One extra webhook header.
#[derive(Clone, Deserialize, Serialize)]
pub struct HttpHeader {
    /// header name
    pub name: String,
    /// header value, sent as typed
    pub value: String,
}

/// One delivery attempt of one SMS to one rule (a retry rewrites its failed line).
#[derive(Clone, Deserialize, Serialize)]
pub struct ForwardLogEntry {
    /// Device clock of the attempt.
    pub timestamp: i64,
    /// Device SMS id.
    pub sms_id: u64,
    /// Decoded sender.
    pub sender: String,
    /// First ~80 bytes of the text, for the list.
    pub content_preview: String,
    /// Rule name at the time.
    pub rule_name: String,
    /// `telegram`, `webhook`, …
    pub destination_type: String,
    /// Delivered.
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// What went wrong, when it did.
    pub error: Option<String>,
    /// The rule, for a manual retry (0 in very old files).
    #[serde(default)]
    pub rule_id: u32,
    /// Full text, for a manual retry.
    #[serde(default)]
    pub content: String,
    /// The SMS's own date field, for a manual retry.
    #[serde(default)]
    pub date: String,
}

/// An SMS ready to send on: number and text decoded from the device's hex.
pub(crate) struct DecodedSms {
    /// device id (0 for the test message)
    pub(crate) id: u64,
    /// plain sender number or name
    pub(crate) sender: String,
    /// plain text
    pub(crate) content: String,
    /// the raw device date string
    pub(crate) date: String,
}
