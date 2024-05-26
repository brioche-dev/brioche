ALTER TABLE builds RENAME TO resolves;

DROP INDEX builds_input_hash;
CREATE UNIQUE INDEX resolves_input_hash
ON resolves (
    input_hash
);
