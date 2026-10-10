#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusAccountDisplay {
    Native {
        provider: String,
        account_id: String,
    },
    ChatGpt {
        email: Option<String>,
        plan: Option<String>,
    },
    ApiKey,
}
