.PHONY: update-db-schema
update-db-schema:
	$(MAKE) -C crates/brioche-core update-db-schema

.PHONY: check-db-schema
check-db-schema:
	$(MAKE) -C crates/brioche-core check-db-schema
