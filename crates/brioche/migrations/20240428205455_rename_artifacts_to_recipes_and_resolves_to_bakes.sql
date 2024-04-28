-- Rename `artifacts` table to `recipes`
-- - Rename `artifact_hash` column to `recipe_hash`
-- - Rename `artifact_json` column to `recipe_json`
CREATE TABLE recipes (
    recipe_hash TEXT PRIMARY KEY NOT NULL,
    recipe_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

INSERT INTO recipes (recipe_hash, recipe_json, created_at)
SELECT artifact_hash, artifact_json, created_at
FROM artifacts;

DROP TABLE artifacts;

-- Rename `project_resolves` table to `project_bakes`
-- - Rename `artifact_hash` column to `recipe_hash`
CREATE TABLE project_bakes (
    id INTEGER PRIMARY KEY NOT NULL,
    project_hash TEXT NOT NULL,
    export TEXT NOT NULL,
    recipe_hash TEXT NOT NULL,
    meta_json TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

CREATE UNIQUE INDEX project_bakes_project_hash_export_recipe_hash
ON project_bakes (
    project_hash,
    export,
    recipe_hash
);

INSERT INTO project_bakes (project_hash, export, recipe_hash, meta_json, created_at)
SELECT project_hash, export, artifact_hash, meta_json, created_at
FROM project_resolves;

DROP INDEX project_resolves_project_hash_export_artifact_hash;
DROP TABLE project_resolves;

-- Rename `child_resolves` table to `child_bakes`
-- - Rename `artifact_hash` column to `recipe_hash`
CREATE TABLE child_bakes (
    id INTEGER PRIMARY KEY NOT NULL,
    parent_hash TEXT NOT NULL,
    recipe_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

CREATE UNIQUE INDEX child_bakes_parent_hash_recipe_hash
ON child_bakes (
    parent_hash,
    recipe_hash
);

INSERT INTO child_bakes (parent_hash, recipe_hash, created_at)
SELECT parent_hash, artifact_hash, created_at
FROM child_resolves;

DROP INDEX child_resolves_parent_hash_artifact_hash;
DROP TABLE child_resolves;

-- Rename `resolves` table to `bakes`
CREATE TABLE bakes (
    id INTEGER PRIMARY KEY NOT NULL,
    input_hash TEXT NOT NULL REFERENCES recipes (recipe_hash),
    output_hash TEXT NOT NULL REFERENCES recipes (recipe_hash),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX bakes_input_hash_output_hash
ON bakes (input_hash, output_hash);

INSERT INTO bakes (input_hash, output_hash, created_at)
SELECT input_hash, output_hash, created_at
FROM resolves;

DROP INDEX resolves_input_hash_output_hash;
DROP TABLE resolves;
