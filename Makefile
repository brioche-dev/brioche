.PHONY: update-db-schema
update-db-schema:
	mkdir -p ./sqlx
	DATABASE_URL=sqlite://./sqlx/schema.db?mode=rwc cargo sqlx migrate run
	DATABASE_URL=sqlite://./sqlx/schema.db cargo sqlx prepare

.PHONY: check-db-schema
check-db-schema:
	mkdir -p ./sqlx
	DATABASE_URL=sqlite://./sqlx/schema.db?mode=rwc cargo sqlx migrate run
	DATABASE_URL=sqlite://./sqlx/schema.db cargo sqlx prepare --check
