DROP TABLE IF EXISTS resolves_old;

ALTER TABLE resolves RENAME TO resolves_old;
DROP INDEX resolves_input_hash;

CREATE TABLE resolves (
    id INTEGER PRIMARY KEY NOT NULL,
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
    input_json,
    input_hash,
    output_json,
    output_hash,
    created_at
)
SELECT
    input_json,
    input_hash,
    output_json,
    output_hash,
    created_at
FROM resolves_old;

DROP TABLE resolves_old;
