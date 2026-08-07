CREATE TABLE squads (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    mission TEXT NOT NULL CHECK(length(mission) > 0),
    state TEXT NOT NULL CHECK(state IN ('active', 'archived')),
    created_at INTEGER NOT NULL,
    archived_at INTEGER,
    CHECK (
        (state = 'active' AND archived_at IS NULL)
        OR (state = 'archived' AND archived_at IS NOT NULL)
    )
) STRICT;

CREATE TABLE agents (
    id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE memberships (
    id TEXT PRIMARY KEY,
    squad_id TEXT NOT NULL REFERENCES squads(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    role TEXT NOT NULL CHECK(length(role) > 0),
    joined_at INTEGER NOT NULL,
    left_at INTEGER,
    UNIQUE(id, squad_id)
) STRICT;

CREATE TABLE instances (
    id TEXT PRIMARY KEY,
    membership_id TEXT NOT NULL REFERENCES memberships(id),
    mode TEXT NOT NULL CHECK(mode IN ('cooperative', 'scheduled', 'harnessed')),
    client_kind TEXT NOT NULL CHECK(length(client_kind) > 0),
    hostname TEXT,
    resume_token_hash BLOB NOT NULL CHECK(length(resume_token_hash) = 32),
    availability TEXT NOT NULL CHECK(availability IN ('idle', 'busy', 'blocked', 'unknown')),
    availability_source TEXT NOT NULL CHECK(length(availability_source) > 0),
    availability_observed_at INTEGER NOT NULL,
    lease_expires_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    closed_at INTEGER
) STRICT;

CREATE TABLE messages (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL,
    squad_id TEXT NOT NULL,
    sender_membership_id TEXT NOT NULL,
    recipient_membership_id TEXT NOT NULL,
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) BETWEEN 1 AND 65536),
    body_hash BLOB NOT NULL CHECK(length(body_hash) = 32),
    priority TEXT NOT NULL CHECK(priority IN ('normal', 'high')),
    reply_to TEXT,
    correlation_id TEXT,
    dedupe_key TEXT,
    created_at INTEGER NOT NULL,
    acknowledged_at INTEGER,
    UNIQUE(id),
    UNIQUE(id, squad_id),
    FOREIGN KEY(sender_membership_id, squad_id) REFERENCES memberships(id, squad_id),
    FOREIGN KEY(recipient_membership_id, squad_id) REFERENCES memberships(id, squad_id),
    FOREIGN KEY(reply_to, squad_id) REFERENCES messages(id, squad_id)
) STRICT;
