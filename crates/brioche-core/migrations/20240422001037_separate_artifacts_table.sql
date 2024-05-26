ALTER TABLE resolves RENAME TO resolves_old;

CREATE TABLE artifacts (
    artifact_hash TEXT PRIMARY KEY NOT NULL,
    artifact_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

CREATE TABLE resolves (
    id INTEGER PRIMARY KEY NOT NULL,
    input_hash TEXT NOT NULL REFERENCES artifacts (artifact_hash),
    output_hash TEXT NOT NULL REFERENCES artifacts (artifact_hash),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX resolves_input_hash_output_hash
ON resolves (input_hash, output_hash);

INSERT INTO artifacts (artifact_hash, artifact_json)
SELECT DISTINCT input_hash, input_json
FROM resolves_old;

INSERT INTO artifacts (artifact_hash, artifact_json)
SELECT DISTINCT output_hash, output_json
FROM resolves_old;

INSERT INTO resolves (input_hash, output_hash)
SELECT input_hash, output_hash
FROM resolves_old;

DROP TABLE resolves_old;
