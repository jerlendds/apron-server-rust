use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub kind: String,
    pub rooms: Vec<String>,
    pub value: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Retry {
    pub method: String,
    pub params: Value,
    pub result: Value,
    pub committed_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub watermark: u64,
    pub next_guest: u64,
    pub users: BTreeMap<String, Value>,
    pub authorization_codes: BTreeMap<String, AuthorizationCode>,
    pub external_identities: BTreeMap<String, String>,
    pub email_addresses: BTreeMap<String, String>,
    pub email_proposals: BTreeMap<String, EmailProposal>,
    pub email_rate: BTreeMap<String, Vec<u64>>,
    pub passkeys: BTreeMap<String, StoredPasskey>,
    pub token_bindings: BTreeMap<String, TokenBinding>,
    pub tokens: BTreeMap<String, String>,
    pub rooms: BTreeMap<String, Value>,
    pub members: BTreeMap<String, BTreeSet<String>>,
    pub messages: BTreeMap<String, Value>,
    pub reactions: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(skip_serializing)]
    pub records: BTreeMap<u64, Record>,
    pub embed_grants: BTreeMap<String, EmbedGrant>,
    pub avatar_assets: BTreeMap<String, String>,
    pub assets: BTreeMap<String, Asset>,
    pub push_registrations: BTreeMap<String, BTreeMap<String, PushRegistration>>,
    pub read_cursors: BTreeMap<String, BTreeMap<String, u64>>,
    pub mutes: BTreeMap<String, BTreeMap<String, u64>>,
    #[serde(skip)]
    pub(crate) room_log: BTreeMap<String, BTreeSet<u64>>,
    pub retries: BTreeMap<String, BTreeMap<String, Retry>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmbedGrant {
    pub embed_id: String,
    pub kind: String,
    pub target: EmbedTarget,
    pub expires: u64,
    pub claimed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum EmbedTarget {
    Message(String),
    Avatar(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub bytes: Vec<u8>,
    pub media_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmailProposal {
    pub address: String,
    pub account: Option<String>,
    pub state: Option<String>,
    pub code_hash: String,
    pub expires: u64,
    pub attempts: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthorizationCode {
    pub binding: AuthorizationBinding,
    #[serde(default)]
    pub pending_passkey: Option<PendingPasskey>,
    pub method: String,
    pub subject: String,
    pub account: Option<String>,
    pub expires: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthorizationBinding {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub resource: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredPasskey {
    pub user: String,
    pub handle: String,
    pub credential: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenBinding {
    pub origin: String,
    pub expires: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingPasskey {
    pub id: String,
    pub key: StoredPasskey,
    pub profile: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushRegistration {
    pub kind: String,
    pub url: String,
    pub token: Option<String>,
    pub keys: Option<PushKeys>,
    pub push_id: Option<String>,
    pub wake: BTreeSet<String>,
    pub version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushKeys {
    pub p256dh: String,
    pub auth: String,
}
