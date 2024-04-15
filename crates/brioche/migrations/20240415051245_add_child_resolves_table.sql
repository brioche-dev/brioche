CREATE TABLE child_resolves (
    id INTEGER PRIMARY KEY NOT NULL,
    parent_hash TEXT NOT NULL,
    artifact_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

CREATE UNIQUE INDEX child_resolves_parent_hash_artifact_hash
ON child_resolves (
    parent_hash,
    artifact_hash
);
