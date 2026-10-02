APP_NAME := Needle
APP_ID   := com.kass.needle
BIN_NAME := needle
BUNDLE   := src-tauri/target/release/bundle/macos/$(APP_NAME).app
DEST     := /Applications/$(APP_NAME).app
BIN      := src-tauri/target/release/$(BIN_NAME)
# the app before the rename: quit it too, it shares the Connect device id
OLD_APP_ID   := com.kass.rustspotify
OLD_BIN_NAME := rust-spotify

.PHONY: install run stop

## install: release .app (with its icon) into /Applications, then open it
install:
	npm run tauri build -- --bundles app
	$(MAKE) stop
	@if [ -d "$(DEST)" ]; then mkdir -p /tmp/trash && mv "$(DEST)" "/tmp/trash/$(APP_NAME).app.$$(date +%Y%m%d-%H%M%S)"; fi
	ditto "$(BUNDLE)" "$(DEST)"
	open "$(DEST)"

## run: fresh release build, run as a bare binary: no .app, so no Dock icon = the dev build
run:
	npm run tauri build -- --no-bundle
	$(MAKE) stop
	$(BIN)

## stop: quit every copy, installed (binary may be named Needle) or dev, new name or old (two copies = two "This Mac" speakers with one device id)
stop:
	-@osascript -e 'if application id "$(APP_ID)" is running then tell application id "$(APP_ID)" to quit' >/dev/null 2>&1
	-@osascript -e 'if application id "$(OLD_APP_ID)" is running then tell application id "$(OLD_APP_ID)" to quit' >/dev/null 2>&1
	-@sleep 1; for p in $(APP_NAME) $(BIN_NAME) $(OLD_BIN_NAME); do pkill -x $$p || true; done
