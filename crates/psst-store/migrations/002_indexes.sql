CREATE UNIQUE INDEX memberships_active_name
    ON memberships(squad_id, normalized_name)
    WHERE left_at IS NULL;

CREATE UNIQUE INDEX messages_dedupe
    ON messages(squad_id, sender_membership_id, dedupe_key)
    WHERE dedupe_key IS NOT NULL;

CREATE INDEX messages_inbox
    ON messages(recipient_membership_id, acknowledged_at, priority, sequence);

CREATE INDEX memberships_roster
    ON memberships(squad_id, left_at);

CREATE INDEX instances_lease_expiry
    ON instances(lease_expires_at, closed_at);
