APP_NAME := Needle
APP_ID   := com.kass.needle
BIN_NAME := needle
DEST     := /Applications/$(APP_NAME).app
# builds run on the MacBook Air (`ssh air`), never on this Mac: its disk is small and Rust
# target dirs grew ~40 GB. Only the finished app comes back, into a temp dir.
AIR      := air
REMOTE   := builds/needle
AIR_ENV  := export PATH=/opt/homebrew/bin:$$HOME/.cargo/bin:$$PATH; cd $(REMOTE)
OUT      := /tmp/needle-build
BUNDLE   := $(OUT)/$(APP_NAME).app
BIN      := $(OUT)/$(BIN_NAME)
# the app before the rename: quit it too, it shares the Connect device id
OLD_APP_ID   := com.kass.rustspotify
OLD_BIN_NAME := rust-spotify

.PHONY: install run stop sync test dmg

## sync: copy the source to the Air (no node_modules, no target, no git)
sync:
	ssh $(AIR) 'mkdir -p $(REMOTE)'
	rsync -a --delete --exclude node_modules --exclude src-tauri/target --exclude .git --exclude media --exclude spikes ./ $(AIR):$(REMOTE)/

## test: frontend + backend tests on the Air
test: sync
	ssh $(AIR) '$(AIR_ENV) && npm ci --silent && npx vitest run && cargo test --manifest-path src-tauri/Cargo.toml --quiet'

## install: release Needle.app built on the Air, copied here into /Applications, then opened
install: sync
	ssh $(AIR) '$(AIR_ENV) && npm ci --silent && npx tauri build --bundles app'
	mkdir -p $(OUT)
	rsync -a --delete $(AIR):$(REMOTE)/src-tauri/target/release/bundle/macos/$(APP_NAME).app/ $(BUNDLE)/
	$(MAKE) stop
	@if [ -d "$(DEST)" ]; then mkdir -p /tmp/trash && mv "$(DEST)" "/tmp/trash/$(APP_NAME).app.$$(date +%Y%m%d-%H%M%S)"; fi
	ditto "$(BUNDLE)" "$(DEST)"
	@# LaunchServices refuses (-600) while the old copy is still exiting: wait, then retry once
	@sleep 2; open "$(DEST)" || { sleep 3; open "$(DEST)"; }

## run: fresh release build on the Air, run here as a bare binary: no .app, so no Dock icon = the dev build
run: sync
	ssh $(AIR) '$(AIR_ENV) && npm ci --silent && npx tauri build --no-bundle'
	mkdir -p $(OUT)
	rsync -a $(AIR):$(REMOTE)/src-tauri/target/release/$(BIN_NAME) $(BIN)
	$(MAKE) stop
	$(BIN)

## dmg: the DMG for a release, built on the Air, copied to $(OUT)
dmg: sync
	ssh $(AIR) '$(AIR_ENV) && npm ci --silent && npx tauri build --bundles dmg'
	mkdir -p $(OUT)
	rsync -a "$(AIR):$(REMOTE)/src-tauri/target/release/bundle/dmg/" $(OUT)/dmg/

## stop: quit every copy, installed (binary may be named Needle) or dev, new name or old (two copies = two "This Mac" speakers with one device id)
stop:
	-@osascript -e 'if application id "$(APP_ID)" is running then tell application id "$(APP_ID)" to quit' >/dev/null 2>&1
	-@osascript -e 'if application id "$(OLD_APP_ID)" is running then tell application id "$(OLD_APP_ID)" to quit' >/dev/null 2>&1
	-@sleep 1; for p in $(APP_NAME) $(BIN_NAME) $(OLD_BIN_NAME); do pkill -x $$p || true; done
