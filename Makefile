NAME := durability-check
RAW := target/wasm32-wasip2/release/durability_check.wasm
OUT ?= dist/$(NAME)

.PHONY: build wasm check test

wasm:
	cargo build --release --target wasm32-wasip2

# Packages the mod folder (mod.wasm + mod.json) into dist/; copy it into the client's mods folder.
build: wasm
	mkdir -p $(OUT)
	cp $(RAW) $(OUT)/mod.wasm
	printf '{\n  "name": "$(NAME)",\n  "version": "0.1.2",\n  "wasm": "mod.wasm",\n  "ruleset": {}\n}\n' > $(OUT)/mod.json
	@echo ">> $(OUT)/mod.wasm"

check:
	cargo check --target wasm32-wasip2

test:
	cargo test
