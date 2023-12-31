.PHONY: update-db-schema
update-db-schema:
	$(MAKE) -C crates/brioche update-db-schema

.PHONY: check-db-schema
check-db-schema:
	$(MAKE) -C crates/brioche check-db-schema
