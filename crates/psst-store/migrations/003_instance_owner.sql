CREATE UNIQUE INDEX instances_unclosed_owner
    ON instances(membership_id)
    WHERE closed_at IS NULL;
