CREATE TABLE project_resolves (
    id INTEGER PRIMARY KEY NOT NULL,
    project_hash TEXT NOT NULL,
    export TEXT NOT NULL,
    artifact_hash TEXT NOT NULL,
    meta_json TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

CREATE UNIQUE INDEX project_resolves_project_hash_export_artifact_hash
ON project_resolves (
    project_hash,
    export,
    artifact_hash
);
