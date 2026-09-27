//! Solver notification texts (`docs/messages.md` §3). Solver messages are
//! in English and never include a party's pubkey.

use crate::store::disputes::Initiator;

pub fn new_dispute(dispute_id: &str, initiator: Initiator) -> String {
    format!("New Mostro dispute\ndispute: {dispute_id}\nopened by: {initiator}")
}

pub fn reminder(dispute_id: &str, unattended_secs: i64) -> String {
    let minutes = unattended_secs.max(0) / 60;
    format!("Dispute still unattended ({minutes} min)\ndispute: {dispute_id}")
}

pub fn taken(dispute_id: &str, by_serbero: bool) -> String {
    let by = if by_serbero { "Serbero" } else { "a solver" };
    format!("Dispute taken\ndispute: {dispute_id}\ntaken by: {by}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_match_the_spec() {
        assert_eq!(
            new_dispute("d1", Initiator::Seller),
            "New Mostro dispute\ndispute: d1\nopened by: seller"
        );
        assert_eq!(
            reminder("d1", 32 * 60 + 59),
            "Dispute still unattended (32 min)\ndispute: d1"
        );
        assert_eq!(
            taken("d1", false),
            "Dispute taken\ndispute: d1\ntaken by: a solver"
        );
        assert_eq!(
            taken("d1", true),
            "Dispute taken\ndispute: d1\ntaken by: Serbero"
        );
    }
}
