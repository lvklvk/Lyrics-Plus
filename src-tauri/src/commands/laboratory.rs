#[tauri::command]
pub fn get_laboratory_status(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> LaboratoryStatus {
    state.laboratory.status(&app)
}

#[tauri::command]
pub fn start_laboratory(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<LaboratoryStatus, String> {
    state.laboratory.start(&app)?;
    Ok(state.laboratory.status(&app))
}

#[tauri::command]
pub fn stop_laboratory(app: tauri::AppHandle, state: State<'_, AppState>) -> LaboratoryStatus {
    state.laboratory.stop(&app);
    state.laboratory.status(&app)
}

#[tauri::command]
pub fn set_laboratory_role(
    app: tauri::AppHandle,
    role: LaboratoryRole,
    state: State<'_, AppState>,
) -> Result<LaboratoryStatus, String> {
    state.laboratory.update_role(&app, role)?;
    Ok(state.laboratory.status(&app))
}

#[tauri::command]
pub fn set_laboratory_auto_start(
    app: tauri::AppHandle,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<LaboratoryStatus, String> {
    state.laboratory.set_auto_start(&app, enabled)?;
    Ok(state.laboratory.status(&app))
}

#[tauri::command]
pub fn set_laboratory_server_settings(
    app: tauri::AppHandle,
    settings: LaboratoryServerSettingsInput,
    state: State<'_, AppState>,
) -> Result<LaboratoryStatus, String> {
    state.laboratory.update_server_settings(&app, settings)?;
    Ok(state.laboratory.status(&app))
}

#[tauri::command]
pub fn set_laboratory_client_settings(
    app: tauri::AppHandle,
    settings: LaboratoryClientSettingsInput,
    state: State<'_, AppState>,
) -> Result<LaboratoryStatus, String> {
    state.laboratory.update_client_settings(&app, settings)?;
    Ok(state.laboratory.status(&app))
}

#[tauri::command]
pub fn set_laboratory_server_password(
    app: tauri::AppHandle,
    password: String,
    state: State<'_, AppState>,
) -> Result<LaboratoryStatus, String> {
    state.laboratory.set_server_password(&app, password)?;
    Ok(state.laboratory.status(&app))
}

#[tauri::command]
pub fn reset_laboratory_web_token(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<LaboratoryStatus, String> {
    state.laboratory.reset_web_token(&app)?;
    Ok(state.laboratory.status(&app))
}

#[tauri::command]
pub fn scan_laboratory_servers(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Vec<LaboratoryServerRecord> {
    state.laboratory.scan_servers(&app)
}

#[tauri::command]
pub fn connect_laboratory_server(
    app: tauri::AppHandle,
    input: LaboratoryConnectInput,
    state: State<'_, AppState>,
) -> Result<LaboratoryStatus, String> {
    state.laboratory.connect_to_server(&app, input)?;
    Ok(state.laboratory.status(&app))
}

#[tauri::command]
pub fn retry_laboratory_connection(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<LaboratoryStatus, String> {
    state.laboratory.retry_connection(&app)?;
    Ok(state.laboratory.status(&app))
}

#[tauri::command]
pub fn kick_laboratory_client(
    app: tauri::AppHandle,
    client_id: String,
    state: State<'_, AppState>,
) -> Result<LaboratoryStatus, String> {
    state.laboratory.kick_client(&app, &client_id)?;
    Ok(state.laboratory.status(&app))
}

#[tauri::command]
pub fn forget_laboratory_client(
    app: tauri::AppHandle,
    client_id: String,
    state: State<'_, AppState>,
) -> Result<LaboratoryStatus, String> {
    state.laboratory.forget_client(&app, &client_id)?;
    Ok(state.laboratory.status(&app))
}

#[tauri::command]
pub fn get_laboratory_themes(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Vec<LaboratoryThemeInfo> {
    state.laboratory.status(&app).themes
}

#[tauri::command]
pub fn reveal_laboratory_themes_directory(app: tauri::AppHandle) -> Result<(), String> {
    let themes_directory = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("获取主题目录失败：{error}"))?
        .join("themes");
    std::fs::create_dir_all(&themes_directory)
        .map_err(|error| format!("创建主题目录失败：{error}"))?;
    app.opener()
        .open_path(themes_directory.to_string_lossy(), None::<&str>)
        .map_err(|error| format!("打开主题目录失败：{error}"))
}
