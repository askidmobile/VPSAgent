//! Сборка контекста запроса: системный промпт + memory + env (cwd, git, платформа).

use std::path::Path;

use crate::memory;

/// Собрать системный промпт агента.
pub fn system_prompt(cwd: &Path) -> String {
    let mut prompt = String::new();
    prompt.push_str("Ты — VPSAgent, интерактивный кодинг-агент в CLI.\n\n");
    prompt.push_str("## Окружение\n\n");
    prompt.push_str(&format!("- Платформа: {}\n", std::env::consts::OS));
    prompt.push_str(&format!("- Рабочий каталог: {}\n", cwd.display()));
    if let Ok(username) = std::env::var("USER") {
        prompt.push_str(&format!("- Пользователь: {username}\n"));
    }
    // git status (одна строка для компактности)
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
    {
        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !branch.is_empty() {
                prompt.push_str(&format!("- Git-ветка: {branch}\n"));
            }
        }
    }

    let mem = memory::collect_memory(cwd);
    if !mem.is_empty() {
        prompt.push_str("\n");
        prompt.push_str(&mem);
        prompt.push('\n');
    }

    prompt.push_str(
        "\n## Правила\n\n\
         - Пиши код, который читается как окружающий: совпадай по комментариям, именованию, идиомам.\n\
         - Ссылайся на код как `file_path:line_number`.\n\
         - Все коммуникации и комментарии — на русском; идентификаторы — на английском.\n\
         - Используй инструменты (read/edit/write/shell/grep) для работы с файлами.\n\
         - Для нетривиальных задач — используй `todo`.\n",
    );
    prompt
}
