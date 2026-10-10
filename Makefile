APP_NAME := Stylus
APP_ID   := com.kass.stylus
BIN_NAME := stylus
DEST     := /Applications/$(APP_NAME).app
# builds run on the MacBook Air (`ssh air`), never on this Mac: its disk is small and Rust
# target dirs grew ~40 GB. Only the finished app comes back, into a temp dir.
AIR      := air
REMOTE   := builds/stylus
AIR_ENV  := export PATH=/opt/homebrew/bin:$$HOME/.cargo/bin:$$PATH; cd $(REMOTE)
OUT      := /tmp/stylus-build
BUNDLE   := $(OUT)/$(APP_NAME).app
BIN      := $(OUT)/$(BIN_NAME)
# the app under its old names (Needle, rust-spotify): quit it too, it shares the Connect device id
OLD_APP_IDS   := com.kass.needle com.kass.rustspotify
OLD_BIN_NAMES := Needle needle rust-spotify

.PHONY: install run stop sync test dmg release

## sync: copy the source to the Air (no node_modules, no target, no git)
sync:
	ssh $(AIR) 'mkdir -p $(REMOTE)'
	rsync -a --delete --exclude node_modules --exclude src-tauri/target --exclude .git --exclude media --exclude spikes ./ $(AIR):$(REMOTE)/

## test: frontend + backend tests on the Air
test: sync
	ssh $(AIR) '$(AIR_ENV) && npm ci --silent && npx vitest run && cargo test --manifest-path src-tauri/Cargo.toml --quiet'

## install: release Stylus.app built on the Air, copied here into /Applications, then opened
install: sync
	ssh $(AIR) '$(AIR_ENV) && npm ci --silent && npx tauri build --bundles app'
	mkdir -p $(OUT)
	rsync -a --delete $(AIR):$(REMOTE)/src-tauri/target/release/bundle/macos/$(APP_NAME).app/ $(BUNDLE)/
	$(MAKE) stop
	@if [ -d "$(DEST)" ]; then mkdir -p /tmp/trash && mv "$(DEST)" "/tmp/trash/$(APP_NAME).app.$$(date +%Y%m%d-%H%M%S)"; fi
	@# the app under its old names: a second copy would be a second "This Mac" speaker
	@for old in Needle rust-spotify; do if [ -d "/Applications/$$old.app" ]; then mkdir -p /tmp/trash && mv "/Applications/$$old.app" "/tmp/trash/$$old.app.$$(date +%Y%m%d-%H%M%S)"; fi; done
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

## dmg: the DMG for a release, built on the Air, copied to $(OUT)/dmg. hdiutil, not Tauri's
## bundle_dmg.sh: that script drives Finder over AppleScript, which fails in an SSH session.
dmg: sync
	ssh $(AIR) '$(AIR_ENV) && npm ci --silent && npx tauri build --bundles app && rm -rf /tmp/stylus-dmg && mkdir -p /tmp/stylus-dmg/src && cp -R src-tauri/target/release/bundle/macos/$(APP_NAME).app /tmp/stylus-dmg/src/ && ln -s /Applications /tmp/stylus-dmg/src/Applications && hdiutil create -volname $(APP_NAME) -srcfolder /tmp/stylus-dmg/src -ov -format UDZO /tmp/stylus-dmg/$(APP_NAME).dmg'
	mkdir -p $(OUT)/dmg
	rsync -a "$(AIR):/tmp/stylus-dmg/$(APP_NAME).dmg" $(OUT)/dmg/
	shasum -a 256 $(OUT)/dmg/$(APP_NAME).dmg

## release: version bump, tag, DMG, release notes and Homebrew cask in one run (scripts/release.py).
## V=0.3.1 NOTES=<file of "- " highlight lines> [DMG=air: build the DMG on the Air, not in CI] [DRY=1: change nothing]
release:
	@test -n "$(V)" -a -n "$(NOTES)" || { echo 'usage: make release V=0.3.1 NOTES=/tmp/notes.md [DMG=air] [DRY=1]'; exit 2; }
	python3 -u scripts/release.py $(V) --notes $(NOTES) --dmg $(or $(DMG),ci) $(if $(DRY),--dry-run)

## stop: quit every copy, installed (binary may be named Stylus) or dev, new name or old (two copies = two "This Mac" speakers with one device id)
stop:
	-@osascript -e 'if application id "$(APP_ID)" is running then tell application id "$(APP_ID)" to quit' >/dev/null 2>&1
	-@for id in $(OLD_APP_IDS); do osascript -e "if application id \"$$id\" is running then tell application id \"$$id\" to quit" >/dev/null 2>&1; done
	-@sleep 1; for p in $(APP_NAME) $(BIN_NAME) $(OLD_BIN_NAMES); do pkill -x $$p || true; done
