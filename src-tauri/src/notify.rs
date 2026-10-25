use tauri::command;
use tauri_plugin_notification::NotificationExt;

#[command]
pub async fn notify(message: String, app: tauri::AppHandle) {
    let _ = app.notification().request_permission();
    let _ = app.notification()
        .builder()
        .title("Stylus")
        .body(&message)
        .show();
}
