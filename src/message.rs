use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Message {
    pub(crate) sender: String,
    pub(crate) receiver: String,
    transaction_id: String,
    block_time: u64,
    message: String,
}

impl Message {
    pub(crate) fn new(
        sender: String,
        receiver: String,
        transaction_id: String,
        block_time: u64,
        message: String,
    ) -> Self {
        Self {
            sender,
            receiver,
            transaction_id,
            block_time,
            message,
        }
    }
}