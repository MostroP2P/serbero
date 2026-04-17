use nostr_sdk::{Client, PublicKey};

use crate::error::{Error, Result};

pub async fn send_gift_wrap_notification(
    client: &Client,
    receiver: &PublicKey,
    message: &str,
) -> Result<()> {
    client
        .send_private_msg(*receiver, message, [])
        .await
        .map_err(|e| Error::Notification(format!("gift-wrap send failed: {e}")))?;
    Ok(())
}
