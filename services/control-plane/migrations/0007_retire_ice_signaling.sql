-- Retires the ICE/STUN/TURN signaling state that Remote Link v1 replaced.
--
-- The tables below stopped having a consumer when the candidate-signaling
-- routes were removed from the service in Objective 1 of the relay
-- replacement. Dropping them here is the forward migration named by the
-- coordinated cutover; no code path reads them and no rollback resurrects
-- them (the archived release keeps its own schema history).
DROP TABLE IF EXISTS turn_credentials;
DROP TABLE IF EXISTS rendezvous_offers;
DROP TABLE IF EXISTS presence;
DROP TABLE IF EXISTS relay_tickets;
DROP TABLE IF EXISTS pairing_requests;

-- Pairings and phone identities enrolled under the retired protocol cannot
-- authenticate a Remote Link: the phone holds a key that was never approved
-- through exact-key enrollment and there is no remote_links row for it.
-- Revoking them (rather than deleting) keeps the audit trail intact and makes
-- "Pairing required" the honest state until the owner re-enrolls explicitly.
-- Host (Mac) identities and the owner account are kept; they carry no grant.
UPDATE pairings
SET revoked_at = NOW()
WHERE revoked_at IS NULL
  AND NOT EXISTS (
    SELECT 1 FROM remote_links rl
    WHERE rl.host_device_id = pairings.host_device_id
      AND rl.client_device_id = pairings.client_device_id
  );

UPDATE devices
SET revoked_at = NOW()
WHERE role = 'client'
  AND revoked_at IS NULL
  AND NOT EXISTS (
    SELECT 1 FROM remote_links rl WHERE rl.client_device_id = devices.id
  );
