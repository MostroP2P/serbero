use nostr_sdk::{Filter, Kind, PublicKey, Timestamp};

const DISPUTE_EVENT_KIND: u16 = 38386;

/// Build the dispute-event subscription filter.
///
/// Real Mostro instances publish kind 38386 events signed by Mostro's
/// own key. The `y` tag carries the platform NAME (e.g. `["mostro"]`
/// or `["mostro", "<instance>"]`), NOT the pubkey — so filtering by
/// `#y=<hex_pubkey>` never matches real events. We filter by the
/// event author instead, matching the approach used by
/// `mostro-watchdog`. Per-status routing (`s=initiated` vs
/// `s=in-progress`) happens in the dispatcher after the event arrives.
///
/// `since` is supplied by the caller. On first startup (empty DB)
/// pass `Timestamp::now()` to avoid replaying historical disputes; on
/// a warm restart pass the last-seen event timestamp (minus a small
/// skew buffer) so disputes published while Serbero was offline are
/// still delivered.
pub fn dispute_filter(mostro_pubkey: &PublicKey, since: Timestamp) -> Filter {
    Filter::new()
        .kind(Kind::Custom(DISPUTE_EVENT_KIND))
        .author(*mostro_pubkey)
        .since(since)
}
