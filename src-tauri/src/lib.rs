// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod activate;
mod api;
mod capture;
mod db;
mod shortcuts;
mod window;
use std::sync::{Arc, Mutex};
use tauri::Manager;

#[cfg(target_os = "macos")]
use tauri::{AppHandle, WebviewWindow};
use tauri_plugin_posthog::{init as posthog_init, PostHogConfig, PostHogOptions};
use tokio::task::JoinHandle;
mod speaker;
use capture::CaptureState;
use speaker::VadConfig;

#[cfg(target_os = "macos")]
#[allow(deprecated)]
use tauri_nspanel::{cocoa::appkit::NSWindowCollectionBehavior, panel_delegate, WebviewWindowExt};

#[derive(Default)]
pub struct AudioState {
    stream_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    vad_config: Arc<Mutex<VadConfig>>,
    is_capturing: Arc<Mutex<bool>>,
}

#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let posthog_api_key = option_env!("POSTHOG_API_KEY").unwrap_or("").to_string();
    
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = app.get_webview_window("main").expect("no main window").set_focus();
        }))
        .plugin(
            tauri_plugin_sql::Builder::default()
                .add_migrations("sqlite:pluely.db", db::migrations())
                .build(),
        )
        .manage(AudioState::default())
        .manage(CaptureState::default())
        .manage(shortcuts::WindowVisibility {
            is_hidden: Mutex::new(false),
        })
        .manage(shortcuts::RegisteredShortcuts::default())
        .manage(shortcuts::LicenseState::default())
        .manage(shortcuts::MoveWindowState::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_keychain::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(posthog_init(PostHogConfig {
            api_key: posthog_api_key,
            options: Some(PostHogOptions {
                disable_session_recording: Some(true),
                capture_pageview: Some(false),
                capture_pageleave: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .plugin(tauri_plugin_machine_uid::init());

    builder
        .invoke_handler(tauri::generate_handler![
            get_app_version,
            window::set_window_height,
            window::open_dashboard,
            window::toggle_dashboard,
            window::move_window,
            capture::capture_to_base64,
            capture::start_screen_capture,
            capture::capture_selected_area,
            capture::close_overlay_window,
            shortcuts::check_shortcuts_registered,
            shortcuts::get_registered_shortcuts,
            shortcuts::update_shortcuts,
            shortcuts::validate_shortcut_key,
            shortcuts::set_license_status,
            shortcuts::set_app_icon_visibility,
            shortcuts::set_always_on_top,
            shortcuts::exit_app,
            activate::activate_license_api,
            activate::deactivate_license_api,
            activate::validate_license_api,
            activate::mask_license_key_cmd,
            activate::get_checkout_url,
            activate::secure_storage_save,
            activate::secure_storage_get,
            activate::secure_storage_remove,
            api::transcribe_audio,
            api::chat_stream_response,
            api::fetch_models,
            api::fetch_prompts,
            api::create_system_prompt,
            api::check_license_status,
            api::get_activity,
            speaker::start_system_audio_capture,
            speaker::stop_system_audio_capture,
            speaker::manual_stop_continuous,
            speaker::check_system_audio_access,
            speaker::request_system_audio_access,
            speaker::get_vad_config,
            speaker::update_vad_config,
            speaker::get_capture_status,
            speaker::get_audio_sample_rate,
            speaker::get_input_devices,
            speaker::get_output_devices,
        ])
        .setup(|app| {
            // Setup main window positioning
            window::setup_main_window(app).expect("Failed to setup main window");
            
            // --- GHOST MODE FOR WINDOWS ---
            #[cfg(target_os = "windows")]
            {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{
                    GetWindowLongPtrA, SetWindowLongPtrA, SetWindowPos,
                    GWL_EXSTYLE, WS_EX_TOOLWINDOW, WS_EX_APPWINDOW,
                    HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_NOACTIVATE, SWP_FRAMECHANGED,
                };

                if let Some(window) = app.get_webview_window("main") {
                    // Get raw handle and convert to our windows crate's HWND type
                    let raw_hwnd = window.hwnd().unwrap().0 as *mut std::ffi::c_void;
                    let hwnd = HWND(raw_hwnd);
                    
                    unsafe {
                        // 1. Modify Extended Window Styles to hide from taskbar
                        let mut style = GetWindowLongPtrA(hwnd, GWL_EXSTYLE);
                        style |= WS_EX_TOOLWINDOW.0 as isize;
                        style &= !(WS_EX_APPWINDOW.0 as isize);
                        let _ = SetWindowLongPtrA(hwnd, GWL_EXSTYLE, style);
                        
                        // 2. CRITICAL: Call SetWindowPos with SWP_FRAMECHANGED to apply the style changes
                        // This forces Windows to re-evaluate the window and update the taskbar
                        let _ = SetWindowPos(
                            hwnd,
                            HWND_TOPMOST,
                            0, 0, 0, 0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED
                        );
                    }
                }
            }
            
            #[cfg(target_os = "macos")]
            init(app.app_handle());

            let app_handle = app.handle();
            if app_handle.get_webview_window("dashboard").is_none() {
                let _ = window::create_dashboard_window(&app_handle);
            }

            // Global shortcuts
            app.handle()
                .plugin(
                    tauri_plugin_global_shortcut::Builder::new()
                        .with_handler(move |app, shortcut, event| {
                            use tauri_plugin_global_shortcut::{Shortcut, ShortcutState};
                            let action_id = {
                                let state = app.state::<shortcuts::RegisteredShortcuts>();
                                let registered = state.shortcuts.lock().unwrap();
                                registered.iter().find_map(|(id, s_str)| {
                                    if let Ok(s) = s_str.parse::<Shortcut>() {
                                        if &s == shortcut { return Some(id.clone()); }
                                    }
                                    None
                                })
                            };

                            if let Some(action_id) = action_id {
                                if event.state() == ShortcutState::Pressed {
                                    if let Some(dir) = action_id.strip_prefix("move_window_") {
                                        shortcuts::start_move_window(app, dir);
                                    } else {
                                        shortcuts::handle_shortcut_action(app, &action_id);
                                    }
                                } else if event.state() == ShortcutState::Released {
                                    if let Some(dir) = action_id.strip_prefix("move_window_") {
                                        shortcuts::stop_move_window(app, dir);
                                    }
                                }
                            }
                        })
                        .build(),
                )
                .expect("Failed shortcuts");

            let _ = shortcuts::setup_global_shortcuts(app.handle());
            
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(target_os = "macos")]
#[allow(deprecated, unexpected_cfgs)]
fn init(app_handle: &AppHandle) {
    let window: WebviewWindow = app_handle.get_webview_window("main").unwrap();
    let panel = window.to_panel().unwrap();
    let delegate = panel_delegate!(MyPanelDelegate { window_did_become_key, window_did_resign_key });

    delegate.set_listener(Box::new(move |name: String| {
        if name == "window_did_become_key" {
            println!("[info]: panel becomes key window!");
        }
    }));

    const NSFloatWindowLevel: i32 = 4;
    panel.set_level(NSFloatWindowLevel);
    const NSWindowStyleMaskNonActivatingPanel: i32 = 1 << 7;
    panel.set_style_mask(NSWindowStyleMaskNonActivatingPanel);
    panel.set_delegate(delegate);
}