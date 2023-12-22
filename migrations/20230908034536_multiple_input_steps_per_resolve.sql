ALTER TABLE resolves RENAME TO resolves_old;
DROP INDEX resolves_input_hash;

CREATE TABLE resolves (
    id TEXT PRIMARY KEY NOT NULL,
    series_id TEXT NOT NULL,
    input_json TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    output_json TEXT NOT NULL,
    output_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

CREATE UNIQUE INDEX resolves_input_hash
ON resolves (
    input_hash
);

INSERT INTO resolves (
    id,
    series_id,
    input_json,
    input_hash,
    output_json,
    output_hash,
    created_at
)
SELECT
    id,
    id as series_id,
    input_json,
    input_hash,
    output_json,
    output_hash,
    created_at
FROM resolves_old;
