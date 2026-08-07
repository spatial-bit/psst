CREATE INDEX messages_inbox_order
    ON messages(recipient_membership_id, acknowledged_at, sequence);
