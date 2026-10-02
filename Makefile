APP_NAME := rust-spotify
APP_ID   := com.kass.rustspotify
BUNDLE   := src-tauri/target/release/bundle/macos/$(APP_NAME).app
DEST     := /Applications/$(APP_NAME).app
BIN      := src-tauri/target/release/$(APP_NAME)

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

## stop: quit every copy, installed or dev (two copies = two "The Run" speakers with one device id)
stop:
	-@osascript -e 'if application id "$(APP_ID)" is running then tell application id "$(APP_ID)" to quit' >/dev/null 2>&1
	-@sleep 1; pkill -x $(APP_NAME) || true
