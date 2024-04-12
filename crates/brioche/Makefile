.PHONY: update-db-schema
update-db-schema:
	mkdir -p ./data
	DATABASE_URL=sqlite://$(CURDIR)/data/schema.db?mode=rwc cargo sqlx migrate run
	DATABASE_URL=sqlite://$(CURDIR)/data/schema.db cargo sqlx prepare

.PHONY: check-db-schema
check-db-schema:
	mkdir -p ./data
	DATABASE_URL=sqlite://$(CURDIR)/data/schema.db?mode=rwc cargo sqlx migrate run
	DATABASE_URL=sqlite://$(CURDIR)/data/schema.db cargo sqlx prepare --check
