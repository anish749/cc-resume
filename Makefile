.PHONY: local-install

local-install:
	cargo install --path .
	claude-resume daemon restart
