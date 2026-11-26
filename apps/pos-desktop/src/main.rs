#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod productivity;

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            productivity::day,
            productivity::agenda,
            productivity::agenda_priorities,
            productivity::roadmap,
            productivity::entities,
            productivity::startup_vault,
            productivity::wall_time,
            productivity::preview,
            productivity::apply,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Personal OS");
}
