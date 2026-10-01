-- Rounds are limited per party (docs/judgments.md §4.1): a party who answers
-- quickly must not use up the rounds of the other one. `rounds` stays the
-- session total for reports. Sessions already open keep the limit they had,
-- so each party starts from the session total.
ALTER TABLE sessions ADD COLUMN buyer_rounds INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN seller_rounds INTEGER NOT NULL DEFAULT 0;
UPDATE sessions SET buyer_rounds = rounds, seller_rounds = rounds;
