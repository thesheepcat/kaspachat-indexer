use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub(crate) struct GetPeersQueryParameters {
    pub(crate) address: String,
}
