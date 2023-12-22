CREATE TABLE builds (
    id TEXT PRIMARY KEY NOT NULL,
    input_json TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    output_json TEXT NOT NULL,
    output_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

CREATE UNIQUE INDEX builds_input_hash
ON builds (
    input_hash
);

CREATE TABLE blob_aliases (
    hash TEXT NOT NULL,
    blob_id TEXT NOT NULL
);

CREATE UNIQUE INDEX blob_aliases_hash
ON blob_aliases (
    hash
);
