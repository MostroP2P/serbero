//! What Serbero already sent each party, read from the stored messages, as
//! the policy needs it (`docs/judgments.md` §4.1).

use crate::policy::next::PartyHistory;
use crate::policy::{Asked, template};
use crate::store::messages::{Direction, Message};
use crate::store::sessions::Party;

/// The templates sent so far, and each party's last template, last
/// question, and whether that question still awaits an answer.
/// `language_changed` is left false: it depends on the current turn.
pub fn from_messages(messages: &[Message]) -> (Asked, PartyHistory<'_>, PartyHistory<'_>) {
    let mut asked = Asked::default();
    for message in messages.iter().filter(|m| m.direction == Direction::Out) {
        if let Some(id) = &message.template_id {
            match message.party {
                Party::Buyer => asked.buyer.insert(id.clone()),
                Party::Seller => asked.seller.insert(id.clone()),
            };
        }
    }
    (
        asked,
        party_history(messages, Party::Buyer),
        party_history(messages, Party::Seller),
    )
}

fn party_history(messages: &[Message], party: Party) -> PartyHistory<'_> {
    let theirs = || {
        messages
            .iter()
            .enumerate()
            .filter(move |(_, m)| m.party == party)
    };
    let sent = || theirs().filter(|(_, m)| m.direction == Direction::Out);
    let last_template = sent()
        .filter_map(|(_, m)| m.template_id.as_deref())
        .next_back();
    let last_question = sent()
        .filter_map(|(i, m)| m.template_id.as_deref().map(|t| (i, t)))
        .rfind(|(_, t)| template::is_question(t));
    let last_reply = theirs()
        .filter(|(_, m)| m.direction == Direction::In)
        .map(|(i, _)| i)
        .next_back();
    PartyHistory {
        last_template,
        last_question: last_question.map(|(_, t)| t),
        outstanding: last_question
            .is_some_and(|(asked_at, _)| last_reply.is_none_or(|r| r < asked_at)),
        language_changed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(direction: Direction, party: Party, template_id: Option<&str>) -> Message {
        Message {
            id: 0,
            session_id: "s1".into(),
            direction,
            party,
            template_id: template_id.map(str::to_owned),
            lang: None,
            content: String::new(),
            attachments: 0,
            inner_event_id: String::new(),
            created_at: 0,
        }
    }

    fn out(party: Party, template: &str) -> Message {
        message(Direction::Out, party, Some(template))
    }

    fn reply(party: Party) -> Message {
        message(Direction::In, party, None)
    }

    #[test]
    fn sent_templates_are_collected_per_party() {
        let messages = [
            out(Party::Buyer, template::ASK_BUYER_SENT),
            out(Party::Seller, template::ASK_SELLER_RECEIVED),
            reply(Party::Buyer),
            out(Party::Buyer, template::ASK_BUYER_DETAILS),
        ];

        let (asked, _, _) = from_messages(&messages);

        assert!(asked.sent(Party::Buyer, template::ASK_BUYER_SENT));
        assert!(asked.sent(Party::Buyer, template::ASK_BUYER_DETAILS));
        assert!(!asked.sent(Party::Buyer, template::ASK_SELLER_RECEIVED));
        assert!(asked.sent(Party::Seller, template::ASK_SELLER_RECEIVED));
    }

    #[test]
    fn a_question_is_outstanding_until_the_party_writes() {
        let asked = [out(Party::Seller, template::ASK_SELLER_RECEIVED)];
        let answered = [
            out(Party::Seller, template::ASK_SELLER_RECEIVED),
            reply(Party::Seller),
        ];
        let asked_again = [
            out(Party::Seller, template::ASK_SELLER_RECEIVED),
            reply(Party::Seller),
            out(Party::Seller, template::ASK_SELLER_CHECK_ACCOUNT),
        ];

        assert!(from_messages(&asked).2.outstanding);
        assert!(!from_messages(&answered).2.outstanding);
        assert!(from_messages(&asked_again).2.outstanding);
        assert!(
            !from_messages(&asked).1.outstanding,
            "the buyer was asked nothing"
        );
    }

    #[test]
    fn courtesy_templates_are_last_templates_but_not_questions() {
        let messages = [
            out(Party::Buyer, template::ASK_BUYER_SENT),
            reply(Party::Buyer),
            out(Party::Buyer, template::THANKS_WAITING),
        ];

        let (_, buyer, _) = from_messages(&messages);

        assert_eq!(buyer.last_template, Some(template::THANKS_WAITING));
        assert_eq!(buyer.last_question, Some(template::ASK_BUYER_SENT));
        assert!(
            !buyer.outstanding,
            "the question was answered; thanks asks nothing"
        );
    }
}
