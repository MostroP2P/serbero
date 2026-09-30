//! Solver notification texts (`docs/messages.md` §3). Solver messages are
//! in English and never include a party's pubkey.

use crate::store::disputes::Initiator;

pub fn new_dispute(dispute_id: &str, initiator: Initiator) -> String {
    format!("Dispute {dispute_id} · new\nopened by: {initiator}")
}

pub fn reminder(dispute_id: &str, unattended_secs: i64) -> String {
    let minutes = unattended_secs.max(0) / 60;
    format!("Dispute {dispute_id} · unattended ({minutes} min)")
}

pub fn taken(dispute_id: &str, by_serbero: bool) -> String {
    let by = if by_serbero { "Serbero" } else { "a solver" };
    format!("Dispute {dispute_id} · taken\ntaken by: {by}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_match_the_spec() {
        assert_eq!(
            new_dispute("d1", Initiator::Seller),
            "Dispute d1 · new\nopened by: seller"
        );
        assert_eq!(
            reminder("d1", 32 * 60 + 59),
            "Dispute d1 · unattended (32 min)"
        );
        assert_eq!(taken("d1", false), "Dispute d1 · taken\ntaken by: a solver");
        assert_eq!(taken("d1", true), "Dispute d1 · taken\ntaken by: Serbero");
    }
}
