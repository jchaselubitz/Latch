-- ICE credentials are short-lived connectivity metadata, paired with the
-- existing candidate JSON. They are never device identities or gateway tokens.
ALTER TABLE presence
    ADD COLUMN ice_ufrag TEXT,
    ADD COLUMN ice_pwd TEXT;

ALTER TABLE rendezvous_offers
    ADD COLUMN ice_ufrag TEXT,
    ADD COLUMN ice_pwd TEXT;
